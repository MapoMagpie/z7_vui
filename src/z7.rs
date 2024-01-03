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

use crate::output_format::{Document, PASSWORD_LINE};

#[derive(Debug)]
pub enum Pushment {
    // the option is (col, row), for nvim cursor
    Full(Vec<String>, Option<(usize, usize)>),
    Line(u64, String),
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
}

impl Z7 {
    pub fn new(pusher: mpsc::Sender<Pushment>) -> Self {
        Self {
            document: Arc::new(RwLock::new(Document::new())),
            doc_sender: pusher,
            password: Arc::new(RwLock::new(None)),
            selected_password: Arc::new(RwLock::new(None)),
            stdin_pipe: Arc::new(RwLock::new(None)),
            execute_status: Arc::new(RwLock::new(ExecuteStatus::None)),
        }
    }

    pub async fn start(
        &mut self,
        oper_recv: mpsc::Receiver<Operation>,
        oper_sender: mpsc::Sender<Operation>,
    ) -> tokio::io::Result<()> {
        let (cmd_sender, cmd_recv) = mpsc::channel::<Cmd>(1);
        let (opt_sender, opt_recv) = mpsc::channel::<Option<String>>(1);
        // begin to execute 'list' command first, then output will push to nvim
        cmd_sender.send(Cmd::List).await.expect("cmd sender error");

        let doc_sender = self.doc_sender.clone();
        let doc_sender_wait_close = self.doc_sender.clone();
        let selected_password = self.selected_password.clone();

        let stdin_pipe = self.stdin_pipe.clone();
        let password = self.password.clone();
        let doc_for_cmd = self.document.clone();
        let doc_for_read = self.document.clone();
        let status = self.execute_status.clone();

        let wait_doc_sender_closed = async move {
            doc_sender_wait_close.closed().await;
            info!("doc channel closed");
            tokio::io::Result::<()>::Err(ErrorKind::Other.into())
        };

        try_join!(
            self.executing(cmd_sender, oper_recv),
            executing_cmd(
                cmd_recv,
                opt_sender,
                stdin_pipe,
                password,
                doc_for_cmd,
                status
            ),
            read_document(
                opt_recv,
                doc_sender,
                doc_for_read,
                selected_password,
                oper_sender
            ),
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

    pub async fn executing(
        &mut self,
        cmd_sender: mpsc::Sender<Cmd>,
        oper_recv: mpsc::Receiver<Operation>,
    ) -> tokio::io::Result<()> {
        self.operation_make(cmd_sender, oper_recv).await
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
}

/// allways receive output from commands by opt_recv
/// then push document to nvim through doc_sender
async fn read_document(
    mut opt_recv: mpsc::Receiver<Option<String>>,
    doc_sender: mpsc::Sender<Pushment>,
    doc: Arc<RwLock<Document>>,
    selected_password: Arc<RwLock<Option<String>>>,
    oper_sender: mpsc::Sender<Operation>,
) -> tokio::io::Result<()> {
    while let Some(line) = opt_recv.recv().await {
        match line {
            Some(line) => {
                // info!("recv output: {}", line);
                {
                    let mut doc = doc.write().await;
                    doc.input(line.as_str());
                    doc.files();
                }
                if line.starts_with("Enter password") {
                    let lines = {
                        let doc = doc.read().await;
                        doc.output()
                    };
                    let selected_password = {
                        let mut selected_password = selected_password.write().await;
                        selected_password.take()
                    };
                    if let Err(e) = doc_sender
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
                    let doc = doc.read().await;
                    doc.output()
                };
                if let Err(e) = doc_sender.send(Pushment::Full(lines, None)).await {
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

/// allways receive commands from cmd_recv
async fn executing_cmd(
    mut cmd_recv: mpsc::Receiver<Cmd>,
    opt_sender: mpsc::Sender<Option<String>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    password: Arc<RwLock<Option<String>>>,
    document: Arc<RwLock<Document>>,
    status: Arc<RwLock<ExecuteStatus>>,
) -> tokio::io::Result<()> {
    while let Some(cmd) = cmd_recv.recv().await {
        info!("recv cmd : {:?}", cmd);
        let opt_sender = opt_sender.clone();
        let stdin_pipe = stdin_pipe.clone();
        let password = password.clone();
        {
            info!("set status to pedding start");
            let mut status = status.write().await;
            *status = ExecuteStatus::Pedding;
            info!("set status to pedding end");
        }
        match cmd {
            Cmd::List => {
                {
                    let mut doc = document.write().await;
                    doc.layout_list();
                }
                info!("command list start");
                let exit_status = execute_list("test.7z", opt_sender, stdin_pipe, password).await?;
                info!("command list finished, status {:?}", exit_status);
                if exit_status.success() {
                    let mut doc = document.write().await;
                    doc.input("Save password");
                    let mut status = status.write().await;
                    *status = ExecuteStatus::None;
                } else {
                    let mut status = status.write().await;
                    *status = ExecuteStatus::List(exit_status);
                }
            }
            Cmd::Extract => {
                {
                    let mut doc = document.write().await;
                    doc.layout_extract();
                }
                let exit_status =
                    execute_extract("test.7z", opt_sender, stdin_pipe, password).await?;
                if exit_status.success() {
                    let mut doc = document.write().await;
                    doc.input("Save password");
                    let mut status = status.write().await;
                    *status = ExecuteStatus::None;
                } else {
                    let mut status = status.write().await;
                    *status = ExecuteStatus::Extract(exit_status);
                }
            }
        };
    }
    info!("cmd recv closed");
    Ok(())
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
    opt_sender: mpsc::Sender<Option<String>>,
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
    opt_sender: mpsc::Sender<Option<String>>,
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
    opt_sender: mpsc::Sender<Option<String>>,
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
    opt_sender: mpsc::Sender<Option<String>>,
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
                        .send(Some(str[from].clone()))
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
                        .send(Some(str[from].clone()))
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
