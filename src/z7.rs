use std::{ffi::OsStr, io::ErrorKind, process::Stdio, sync::Arc};

use log::{error, info};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, ChildStdin, ChildStdout, Command},
    select,
    sync::{
        mpsc::{self},
        RwLock,
    },
    try_join,
};

use crate::output_format::{Document, Lines, PASSWORD_LINE};

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
    Execute,
    Retry,
}

#[derive(Debug)]
pub enum Cmd {
    List,
    Extract,
}

pub struct Z7 {
    document: Arc<RwLock<Document>>,
    doc_sender: mpsc::Sender<Pushment>,
    password: Arc<RwLock<Option<String>>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    need_password: Arc<RwLock<bool>>,
}

impl Z7 {
    pub fn new(pusher: mpsc::Sender<Pushment>) -> Self {
        Self {
            document: Arc::new(RwLock::new(Document::new())),
            doc_sender: pusher,
            password: Arc::new(RwLock::new(None)),
            stdin_pipe: Arc::new(RwLock::new(None)),
            need_password: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn start(&mut self, oper_recv: mpsc::Receiver<Operation>) -> tokio::io::Result<()> {
        let (cmd_sender, cmd_recv) = mpsc::channel::<Cmd>(1);
        let (opt_sender, opt_recv) = mpsc::channel::<Option<String>>(1);

        let doc = self.document.clone();
        let doc_pusher = self.doc_sender.clone();
        let need_password = self.need_password.clone();

        // begin to execute 'list' command first, then output will push to nvim
        cmd_sender.send(Cmd::List).await.expect("cmd sender error");
        try_join!(
            self.executing(cmd_sender, cmd_recv, opt_sender, oper_recv),
            read_document(opt_recv, doc_pusher, doc, need_password)
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
                    if let Err(e) = cmd_sender.send(Cmd::List).await {
                        error!("send cmd error: {}", e);
                        return Err(ErrorKind::BrokenPipe.into());
                    }
                }
                Operation::Password(pwd) => {
                    {
                        let mut password = self.password.write().await;
                        password.replace(pwd.clone());
                    }
                    {
                        let mut doc = self.document.write().await;
                        doc.input(format!("Input password: {}", pwd).as_str());
                    }
                    self.write_password(&pwd).await;
                }
            }
        }
        info!("operation recv closed");
        Ok(())
    }

    pub async fn executing(
        &mut self,
        cmd_sender: mpsc::Sender<Cmd>,
        cmd_recv: mpsc::Receiver<Cmd>,
        opt_sender: mpsc::Sender<Option<String>>,
        oper_recv: mpsc::Receiver<Operation>,
    ) -> tokio::io::Result<()> {
        let stdin_pipe = self.stdin_pipe.clone();
        let password = self.password.clone();

        let doc = self.document.clone();
        match try_join!(
            self.operation_make(cmd_sender, oper_recv),
            executing_cmd(cmd_recv, opt_sender, stdin_pipe, password, doc),
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// write password to child stdin,
    /// then child will continue to execute with output
    async fn write_password(&mut self, pwd: &str) {
        {
            let mut need_password = self.need_password.write().await;
            if !*need_password {
                info!("Do not need password");
                return;
            } else {
                *need_password = false;
            }
        }
        let mut stdin = self.stdin_pipe.write().await;
        // will set stdin to None
        if let Some(mut pipe) = stdin.take() {
            info!("writed password: {}", pwd);
            pipe.write_all(pwd.as_bytes()).await.unwrap();
        } else {
            error!("7z command stdin pipe is none");
        }
    }
}

/// allways receive output from commands by opt_recv
/// then push document to nvim through doc_sender
async fn read_document(
    mut opt_recv: mpsc::Receiver<Option<String>>,
    doc_sender: mpsc::Sender<Pushment>,
    doc: Arc<RwLock<Document>>,
    need_password: Arc<RwLock<bool>>,
) -> tokio::io::Result<()> {
    while let Some(line) = opt_recv.recv().await {
        match line {
            Some(line) => {
                info!("recv output: {}", line);
                {
                    let mut doc = doc.write().await;
                    doc.input(line.as_str());
                }
                if line.starts_with("Enter password") {
                    let mut np = need_password.write().await;
                    *np = true;
                    let lines = {
                        let doc = doc.read().await;
                        doc.output()
                    };
                    if let Err(e) = doc_sender
                        .send(Pushment::Full(lines, Some((PASSWORD_LINE, 1))))
                        .await
                    {
                        info!("pushment sender error: {}", e);
                        return Err(ErrorKind::Interrupted.into());
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
) -> tokio::io::Result<()> {
    while let Some(cmd) = cmd_recv.recv().await {
        let password = password.clone();
        info!("recv cmd : {:?}", cmd);
        let opt_sender = opt_sender.clone();
        let stdin_pipe = stdin_pipe.clone();
        match cmd {
            Cmd::List => {
                {
                    let mut doc = document.write().await;
                    doc.layout_list();
                }
                execute_list("test.7z", opt_sender, stdin_pipe, password).await?;
                info!("command list finished");
            }
            Cmd::Extract => {
                {
                    let mut doc = document.write().await;
                    doc.layout_extract();
                }
                execute_extract("test.7z", opt_sender, stdin_pipe, password).await?;
                info!("command extract finished");
            }
        }
    }
    info!("cmd recv closed");
    Ok(())
}

fn spawn_cmd<I>(
    args: I,
) -> tokio::io::Result<(Option<ChildStdin>, Option<ChildStdout>, Option<ChildStderr>)>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    let mut child = Command::new("7z")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    Ok((child.stdin.take(), child.stdout.take(), child.stderr.take()))
}

async fn execute_list(
    filename: &str,
    opt_sender: mpsc::Sender<Option<String>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    password: Arc<RwLock<Option<String>>>,
) -> tokio::io::Result<()> {
    let mut args: Vec<String> = vec!["l".to_string(), filename.to_string()];
    {
        let pwd = password.read().await;
        if let Some(pwd) = pwd.as_ref() {
            args.push(format!("-p{}", pwd));
        }
    }
    let (stdin, stdout, stderr) = spawn_cmd(args)?;
    // set stdin to Z7.stdin_pipe
    stdin_pipe.write().await.replace(stdin.unwrap());

    read_output(stdout.unwrap(), stderr.unwrap(), opt_sender.clone()).await
}

async fn execute_extract(
    filename: &str,
    opt_sender: mpsc::Sender<Option<String>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    password: Arc<RwLock<Option<String>>>,
) -> tokio::io::Result<()> {
    let mut args: Vec<String> = vec!["x".to_string(), filename.to_string(), "-y".to_string()];
    {
        let pwd = password.read().await;
        if let Some(pwd) = pwd.as_ref() {
            args.push(format!("-p{}", pwd));
        }
    }
    let (stdin, stdout, stderr) = spawn_cmd(args)?;
    // set stdin to Z7.stdin_pipe
    stdin_pipe.write().await.replace(stdin.unwrap());

    read_output(stdout.unwrap(), stderr.unwrap(), opt_sender.clone()).await
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
