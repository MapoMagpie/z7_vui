use std::{
    ffi::OsStr,
    io::{stdout, ErrorKind},
    ops::Deref,
    process::Stdio,
    str::FromStr,
    sync::Arc,
};

use log::{error, info};
use ropey::Rope;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{
        mpsc::{self},
        RwLock,
    },
    try_join,
};

#[derive(Debug)]
pub enum Pushment {
    Full(Vec<String>),
    Line(u64, String),
    None,
}

#[derive(Debug)]
pub enum Operation {
    Password(String),
    Execute,
}

#[derive(Debug)]
pub enum Cmd {
    List,
    Extract,
}

pub struct Z7 {
    document: Arc<RwLock<Rope>>,
    doc_sender: mpsc::Sender<Pushment>,
    password: Arc<RwLock<Option<String>>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    need_password: Arc<RwLock<bool>>,
}

impl Z7 {
    pub fn new(pusher: mpsc::Sender<Pushment>) -> Self {
        let rope = Rope::new();
        Self {
            document: Arc::new(RwLock::new(rope)),
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
                Operation::Password(pwd) => {
                    {
                        let mut password = self.password.write().await;
                        password.replace(pwd.clone());
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

        match try_join!(
            self.operation_make(cmd_sender, oper_recv),
            executing_cmd(cmd_recv, opt_sender, stdin_pipe, password),
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
    doc: Arc<RwLock<Rope>>,
    need_password: Arc<RwLock<bool>>,
) -> tokio::io::Result<()> {
    while let Some(line) = opt_recv.recv().await {
        match line {
            Some(mut line) => {
                info!("recv output: {}", line);
                line.push('\n');
                {
                    let mut doc = doc.write().await;
                    doc.append(line.clone().into());
                }
                if line.starts_with("Enter password") {
                    let mut np = need_password.write().await;
                    *np = true;
                    let lines = {
                        let doc = doc.read().await;
                        doc.lines()
                            .map(|l| l.to_string().trim_end().to_string())
                            .collect()
                    };
                    let _ = doc_sender.send(Pushment::Full(lines)).await;
                }
            }
            // "None" means a command is finished, but we still wait for other commands output
            None => {
                let lines = {
                    let mut doc = doc.write().await;
                    let lines = doc
                        .lines()
                        .map(|l| l.to_string().trim_end().to_string())
                        .collect();
                    doc.remove(0..);
                    lines
                };
                let _ = doc_sender.send(Pushment::Full(lines)).await;
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
) -> tokio::io::Result<()> {
    while let Some(cmd) = cmd_recv.recv().await {
        let password = password.clone();
        info!("recv cmd : {:?}", cmd);
        let opt_sender = opt_sender.clone();
        let stdin_pipe = stdin_pipe.clone();
        match cmd {
            Cmd::List => {
                execute_list("test.7z", opt_sender, stdin_pipe, password).await?;
            }
            Cmd::Extract => {
                execute_extract("test.7z", opt_sender, stdin_pipe, password).await?;
            }
        }
    }
    info!("cmd recv closed");
    Ok(())
}

fn spawn_cmd<I>(
    args: I,
    stdout: Option<Stdio>,
) -> tokio::io::Result<(Option<ChildStdin>, Option<ChildStdout>, Child)>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    let is_stdout_some = stdout.is_some();
    let mut child = Command::new("7z")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(stdout.unwrap_or(Stdio::piped()))
        .spawn()?;
    Ok((
        child.stdin.take(),
        if is_stdout_some {
            None
        } else {
            child.stdout.take()
        },
        child,
    ))
}

async fn execute_list(
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
    let (stdin, stdout, _) = spawn_cmd(args, None)?;
    // set stdin to Z7.stdin_pipe
    stdin_pipe.write().await.replace(stdin.unwrap());

    read_output(stdout.unwrap(), opt_sender).await
}

async fn execute_extract(
    filename: &str,
    _opt_sender: mpsc::Sender<Option<String>>,
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
    let (stdin, _, mut child) = spawn_cmd(args, Some(stdout().into()))?;
    // set stdin to Z7.stdin_pipe
    stdin_pipe.write().await.replace(stdin.unwrap());
    let _ = child.wait().await;
    Ok(())
}

async fn read_output(
    mut stdout: ChildStdout,
    opt_sender: mpsc::Sender<Option<String>>,
) -> tokio::io::Result<()> {
    let mut str = String::new();
    loop {
        match stdout.read_u8().await {
            Ok(c) => {
                // '\n'
                if c == 0x0a {
                    opt_sender
                        .send(Some(str.clone()))
                        .await
                        .expect("send string line error");
                    str.clear();
                }
                // ':'
                else if c == 0x3a && str.starts_with("Enter password") {
                    str.push(c as char);
                    opt_sender
                        .send(Some(str.clone()))
                        .await
                        .expect("send string line error");
                    str.clear();
                } else {
                    str.push(c as char);
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
