use std::{
    ffi::OsStr,
    io::ErrorKind,
    process::{ExitStatus, Stdio},
    sync::Arc,
    vec,
};

use log::{error, info};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    select,
    sync::{
        mpsc::{self},
        RwLock,
    },
    try_join,
};

use crate::{
    options::Options,
    output_format::{Document, PASSWORD_LINE},
};

#[derive(Debug)]
pub enum Pushment {
    // the option is (col, row), for nvim cursor
    Full(Vec<String>, Option<(usize, usize)>),
    #[allow(dead_code)]
    Line(u64, String),
    #[allow(dead_code)]
    None,
}

#[derive(Debug)]
pub enum Operation {
    Password(String),
    SelectPassword(String),
    Execute,
    Retry,
}

#[derive(Debug)]
pub enum Cmd {
    List,
    Extract,
}

#[derive(Debug)]
pub enum ExecuteStatus {
    List(ExitStatus),
    Extract(ExitStatus),
    None,
    Pedding,
}

pub struct Z7 {
    document: Arc<RwLock<Document>>,
    doc_sender: mpsc::Sender<Pushment>,
    password: Arc<RwLock<Option<String>>>,
    selected_password: Arc<RwLock<Option<String>>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    execute_status: Arc<RwLock<ExecuteStatus>>,
    options: Arc<RwLock<Options>>,
}

impl Clone for Z7 {
    fn clone(&self) -> Self {
        Self {
            document: self.document.clone(),
            doc_sender: self.doc_sender.clone(),
            password: self.password.clone(),
            selected_password: self.selected_password.clone(),
            stdin_pipe: self.stdin_pipe.clone(),
            execute_status: self.execute_status.clone(),
            options: self.options.clone(),
        }
    }
}

impl Z7 {
    pub fn new(pusher: mpsc::Sender<Pushment>, opt: Options) -> Self {
        Self {
            document: Arc::new(RwLock::new(Document::new())),
            doc_sender: pusher,
            password: Arc::new(RwLock::new(None)),
            selected_password: Arc::new(RwLock::new(None)),
            stdin_pipe: Arc::new(RwLock::new(None)),
            execute_status: Arc::new(RwLock::new(ExecuteStatus::None)),
            options: Arc::new(RwLock::new(opt)),
        }
    }

    pub async fn start(
        &mut self,
        oper_recv: mpsc::Receiver<Operation>,
        oper_sender: mpsc::Sender<Operation>,
    ) -> tokio::io::Result<()> {
        let (cmd_sender, cmd_recv) = mpsc::channel::<Cmd>(1);
        // (line, from stdout:1 or stderr:2)
        let (opt_sender, opt_recv) = mpsc::channel::<Option<(String, usize)>>(1);
        // begin to execute 'list' command first, then output will push to nvim
        cmd_sender.send(Cmd::List).await.expect("cmd sender error");

        let doc_sender_wait_close = self.doc_sender.clone();

        let wait_doc_sender_closed = async move {
            doc_sender_wait_close.closed().await;
            info!("doc channel closed");
            tokio::io::Result::<()>::Err(ErrorKind::Other.into())
        };
        let mut z7_1 = self.clone();
        let mut z7_2 = self.clone();
        try_join!(
            z7_1.operation_make(cmd_sender, oper_recv),
            z7_2.executing_cmd(cmd_recv, opt_sender),
            self.read_document(opt_recv, oper_sender),
            wait_doc_sender_closed
        )
        .map(|_| ())
    }

    pub async fn operation_make(
        &mut self,
        cmd_sender: mpsc::Sender<Cmd>,
        mut oper_recv: mpsc::Receiver<Operation>,
    ) -> tokio::io::Result<()> {
        while let Some(oper) = oper_recv.recv().await {
            info!("recv operation: {:?}", oper);
            match oper {
                Operation::Execute => {
                    if let Err(e) = cmd_sender.send(Cmd::Extract).await {
                        error!("send cmd error: {}", e);
                        return Err(ErrorKind::BrokenPipe.into());
                    }
                }
                Operation::Retry => {
                    {
                        let mut password = self.password.write().await;
                        password.take();
                    }
                    let _ = cmd_sender.try_send(Cmd::List);
                }
                Operation::Password(pwd) => {
                    self.write_password(&pwd).await;
                }
                Operation::SelectPassword(pwd) => {
                    let should_retry = {
                        // info!("check execute status start");
                        let status = self.execute_status.read().await;
                        // info!("recv password current status: {:?}", status);
                        !matches!(*status, ExecuteStatus::Pedding)
                    };
                    if should_retry {
                        {
                            let mut password = self.password.write().await;
                            password.take();
                        }
                        let _ = cmd_sender.send(Cmd::List).await;
                        {
                            let mut selected_password = self.selected_password.write().await;
                            selected_password.replace(pwd);
                        }
                    } else {
                        self.write_password(&pwd).await;
                    }
                }
            }
        }
        info!("operation recv closed");
        Ok(())
    }

    /// write password to child stdin,
    /// then child will continue to execute with output
    async fn write_password(&mut self, pwd: &str) {
        let mut stdin = self.stdin_pipe.write().await;
        // will set stdin to None
        if let Some(mut pipe) = stdin.take() {
            info!("writed password: {}", pwd);
            pipe.write_all(pwd.as_bytes())
                .await
                .expect("write password error");
        } else {
            info!("7z command stdin pipe is none");
        }
        {
            let mut password = self.password.write().await;
            let new_password = pwd.to_string();
            if password.is_some() && password.as_ref().unwrap() == &new_password {
                return;
            }
            password.replace(new_password);
        }
        {
            let mut doc = self.document.write().await;
            doc.input(format!("Input password: {}", pwd).as_str());
        }
    }

    /// allways receive commands from cmd_recv
    async fn executing_cmd(
        &mut self,
        mut cmd_recv: mpsc::Receiver<Cmd>,
        opt_sender: mpsc::Sender<Option<(String, usize)>>,
    ) -> tokio::io::Result<()> {
        while let Some(cmd) = cmd_recv.recv().await {
            info!("recv cmd : {:?}", cmd);
            let opt_sender = opt_sender.clone();
            let stdin_pipe = self.stdin_pipe.clone();
            let password = self.password.clone();
            {
                info!("set status to pedding start");
                let mut status = self.execute_status.write().await;
                *status = ExecuteStatus::Pedding;
                info!("set status to pedding end");
            }
            let (exit_status, cmd) = match cmd {
                Cmd::List => {
                    {
                        let mut doc = self.document.write().await;
                        doc.layout_list();
                    }
                    let file = { self.options.read().await.file.clone() };
                    (
                        execute_list(&file, opt_sender, stdin_pipe, password).await?,
                        Cmd::List,
                    )
                }
                Cmd::Extract => {
                    {
                        let mut doc = self.document.write().await;
                        doc.layout_extract();
                    }
                    let file = { self.options.read().await.file.clone() };
                    (
                        execute_extract(&file, opt_sender, stdin_pipe, password).await?,
                        Cmd::Extract,
                    )
                }
            };
            {
                let mut status = self.execute_status.write().await;
                if exit_status.success() {
                    if let Some(pwd) = self.password.read().await.clone() {
                        let mut doc = self.document.write().await;
                        doc.input(format!("Save password: {}", pwd).as_str());
                    }
                    *status = ExecuteStatus::None;
                } else {
                    self.password.write().await.take();
                    *status = match cmd {
                        Cmd::List => ExecuteStatus::List(exit_status),
                        Cmd::Extract => ExecuteStatus::Extract(exit_status),
                    };
                }
            }
        }
        info!("cmd recv closed");
        Ok(())
    }

    /// allways receive output from commands by opt_recv
    /// then push document to nvim through doc_sender
    async fn read_document(
        &mut self,
        mut opt_recv: mpsc::Receiver<Option<(String, usize)>>,
        oper_sender: mpsc::Sender<Operation>,
    ) -> tokio::io::Result<()> {
        while let Some(line) = opt_recv.recv().await {
            match line {
                Some((line, fd)) => {
                    info!("recv output: {},{}", fd, line);
                    {
                        let mut doc = self.document.write().await;
                        doc.input(line.as_str());
                    }
                    if line.starts_with("Enter password") {
                        {
                            let mut doc = self.document.write().await;
                            doc.input(
                                format!("Password history file: {}", {
                                    self.options.read().await.password_history_file.clone()
                                })
                                .as_str(),
                            );
                        }
                        let lines = {
                            let doc = self.document.read().await;
                            doc.output()
                        };
                        let selected_password = {
                            let mut selected_password = self.selected_password.write().await;
                            selected_password.take()
                        };
                        if let Err(e) = self
                            .doc_sender
                            .send(Pushment::Full(lines, {
                                if selected_password.is_none() {
                                    Some((PASSWORD_LINE, 1))
                                } else {
                                    None
                                }
                            }))
                            .await
                        {
                            info!("pushment sender error: {}", e);
                            return Err(ErrorKind::Interrupted.into());
                        }
                        if let Some(pwd) = selected_password {
                            if let Err(e) = oper_sender.send(Operation::Password(pwd)).await {
                                info!("operation sender error: {}", e);
                                return Err(ErrorKind::Interrupted.into());
                            }
                        }
                    }
                }
                // "None" means a command is finished, but we still wait for other commands output
                None => {
                    let lines = {
                        let doc = self.document.read().await;
                        doc.output()
                    };
                    if let Err(e) = self.doc_sender.send(Pushment::Full(lines, None)).await {
                        info!("pushment sender error: {}", e);
                        return Err(ErrorKind::Interrupted.into());
                    }
                }
            }
        }
        // do not care occur error
        info!("output: finished");
        Ok(())
    }
}

fn spawn_cmd<I>(args: I) -> tokio::io::Result<Child>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    Command::new("7z")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

async fn execute_cmd<I>(
    opt_sender: mpsc::Sender<Option<(String, usize)>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    args: I,
) -> tokio::io::Result<ExitStatus>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    let mut child = spawn_cmd(args)?;
    // set stdin to Z7.stdin_pipe
    stdin_pipe
        .write()
        .await
        .replace(child.stdin.take().unwrap());

    read_output(
        child.stdout.take().unwrap(),
        child.stderr.take().unwrap(),
        opt_sender.clone(),
    )
    .await?;
    child.wait().await
}

async fn execute_list(
    filename: &str,
    opt_sender: mpsc::Sender<Option<(String, usize)>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    password: Arc<RwLock<Option<String>>>,
) -> tokio::io::Result<ExitStatus> {
    let mut args = vec!["l", filename];
    let pwd = { password.read().await.clone() };
    let pwd = pwd.map(|s| format!("-p{}", s));
    if let Some(w) = pwd.as_ref() {
        args.push(w);
    }
    execute_cmd(opt_sender, stdin_pipe, args).await
}

async fn execute_extract(
    filename: &str,
    opt_sender: mpsc::Sender<Option<(String, usize)>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    password: Arc<RwLock<Option<String>>>,
) -> tokio::io::Result<ExitStatus> {
    let mut args = vec!["x", filename, "-y"];
    let pwd = { password.read().await.clone() };
    let pwd = pwd.map(|s| format!("-p{}", s));
    if let Some(w) = pwd.as_ref() {
        args.push(w);
    }
    execute_cmd(opt_sender, stdin_pipe, args).await
}

async fn read_output<O, E>(
    stdout: O,
    stderr: E,
    opt_sender: mpsc::Sender<Option<(String, usize)>>,
) -> tokio::io::Result<()>
where
    O: AsyncReadExt + Unpin,
    E: AsyncReadExt + Unpin,
{
    let mut reader = OutputReader::new(stdout, stderr);
    // stdout , stderr
    let mut str = [String::new(), String::new()];
    loop {
        match reader.read().await {
            Ok((c, from)) => {
                // 'LF'
                if c == 0x0a {
                    opt_sender
                        .send(Some((str[from].clone(), from + 1)))
                        .await
                        .expect("send string line error");
                    str[from].clear();
                }
                // '\b' backspace, actually someone eat them
                else if c == 0x08 {
                    info!("read output has backspace");
                }
                // ':'
                else if c == 0x3a && str[from].starts_with("Enter password") {
                    str[from].push(c as char);
                    opt_sender
                        .send(Some((str[from].clone(), from + 1)))
                        .await
                        .expect("send string line error");
                    str[from].clear();
                } else {
                    str[from].push(c as char);
                    // info!("read output: {}", str);
                }
            }
            // EOF
            Err(e) => {
                opt_sender
                    .send(None)
                    .await
                    .expect("send string line error at end");
                info!("read output eof: {}", e);
                break;
            }
        }
    }
    Ok(())
}

/// read the stdout and stderr from child process
/// hold EOF one of them, util both of them are EOF
struct OutputReader<O, E> {
    stdout: O,
    stderr: E,
    eof: [bool; 2],
}

impl<O, E> OutputReader<O, E> {
    fn new(stdout: O, stderr: E) -> Self {
        Self {
            stdout,
            stderr,
            eof: [false; 2],
        }
    }
}

impl<O, E> OutputReader<O, E>
where
    O: AsyncReadExt + Unpin,
    E: AsyncReadExt + Unpin,
{
    async fn read(&mut self) -> tokio::io::Result<(u8, usize)> {
        let r = select! {
            c = self.stdout.read_u8(), if !self.eof[0] => (c, 0),
            c = self.stderr.read_u8(), if !self.eof[1] => (c, 1),
        };
        match r {
            (Ok(c), p) => Ok((c, p)),
            (Err(e), from) => {
                self.eof[from] = true;
                if self.eof[0] && self.eof[1] {
                    Err(e)
                } else {
                    Ok((0x0a, from))
                }
            }
        }
    }
}
