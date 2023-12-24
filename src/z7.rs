use std::{process::Stdio, sync::Arc};

use log::{error, info};
use ropey::Rope;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    join,
    process::{ChildStdin, ChildStdout, Command},
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
}

#[derive(Debug)]
pub enum Cmd {
    List,
    Extract,
}

pub struct Z7 {
    document: Arc<RwLock<Rope>>,
    doc_sender: mpsc::Sender<Pushment>,
    password: RwLock<Option<String>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    need_password: Arc<RwLock<bool>>,
}

impl Z7 {
    pub fn new(pusher: mpsc::Sender<Pushment>) -> Self {
        let rope = Rope::new();
        Self {
            document: Arc::new(RwLock::new(rope)),
            doc_sender: pusher,
            password: RwLock::new(None),
            stdin_pipe: Arc::new(RwLock::new(None)),
            need_password: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn start(
        &mut self,
        pwd_recv: mpsc::Receiver<Option<String>>,
    ) -> tokio::io::Result<()> {
        let (cmd_sender, cmd_recv) = mpsc::channel::<Cmd>(1);
        let (opt_sender, opt_recv) = mpsc::channel::<Option<String>>(1);

        async fn read_document(
            doc: Arc<RwLock<Rope>>,
            mut opt_rec: mpsc::Receiver<Option<String>>,
            doc_sender: mpsc::Sender<Pushment>,
            need_password: Arc<RwLock<bool>>,
        ) -> tokio::io::Result<()> {
            loop {
                if let Some(line) = opt_rec.recv().await {
                    match line {
                        Some(mut line) => {
                            info!("recv output: {}", line);
                            {
                                line.push('\n');
                                let mut doc = doc.write().await;
                                doc.append(line.clone().into());
                            }
                            if line.starts_with("Enter password") {
                                let mut np = need_password.write().await;
                                *np = true;
                                let doc = doc.read().await;
                                let lines = doc
                                    .lines()
                                    .map(|l| l.to_string().trim_end().to_string())
                                    .collect();
                                let _ = doc_sender.send(Pushment::Full(lines)).await;
                            }
                        }
                        None => {
                            let doc = doc.read().await;
                            let lines = doc
                                .lines()
                                .map(|l| l.to_string().trim_end().to_string())
                                .collect();
                            let _ = doc_sender.send(Pushment::Full(lines)).await;
                        }
                    }
                } else {
                    info!("output: finished");
                    break;
                }
            }
            Ok(())
        }

        cmd_sender.send(Cmd::List).await.expect("cmd sender error");
        let doc = self.document.clone();
        let doc_pusher = self.doc_sender.clone();
        let need_password = self.need_password.clone();
        let _ = join!(
            self.executing(cmd_recv, opt_sender, pwd_recv),
            read_document(doc, opt_recv, doc_pusher, need_password)
        );
        Ok(())
    }

    pub async fn operation_make(
        &mut self,
        mut pwd_recv: mpsc::Receiver<Option<String>>,
    ) -> tokio::io::Result<()> {
        while let Some(Some(pwd)) = pwd_recv.recv().await {
            info!("recv password: {}", pwd);
            {
                let mut password = self.password.write().await;
                password.replace(pwd.clone());
            }

            self.write_password(&pwd).await;
        }
        Ok(())
    }

    pub async fn executing(
        &mut self,
        cmd_recv: mpsc::Receiver<Cmd>,
        opt_sender: mpsc::Sender<Option<String>>,
        pwd_recv: mpsc::Receiver<Option<String>>,
    ) -> tokio::io::Result<()> {
        async fn executing_cmd(
            mut cmd_recv: mpsc::Receiver<Cmd>,
            opt_sender: mpsc::Sender<Option<String>>,
            stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
        ) -> tokio::io::Result<()> {
            while let Some(cmd) = cmd_recv.recv().await {
                info!("recv cmd : {:?}", cmd);
                let opt_sender = opt_sender.clone();
                let stdin_pipe = stdin_pipe.clone();
                match cmd {
                    Cmd::List => {
                        Z7::execute_list("test.7z", opt_sender, stdin_pipe).await?;
                    }
                    Cmd::Extract => {
                        unimplemented!()
                    }
                }
            }
            Ok(())
        }
        let stdin_pipe = self.stdin_pipe.clone();
        match try_join!(
            executing_cmd(cmd_recv, opt_sender, stdin_pipe),
            self.operation_make(pwd_recv)
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn write_password(&mut self, pwd: &str) {
        let mut need_password = self.need_password.write().await;
        if !*need_password {
            info!("no need password");
            return;
        } else {
            *need_password = false;
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

    pub async fn execute_list(
        filename: &str,
        opt_sender: mpsc::Sender<Option<String>>,
        stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    ) -> tokio::io::Result<()> {
        let mut child = Command::new("7z")
            .args(["l", filename])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        stdin_pipe
            .write()
            .await
            .replace(child.stdin.take().unwrap());

        async fn read_and_send(
            mut pipe: Option<ChildStdout>,
            opt_sender: mpsc::Sender<Option<String>>,
        ) -> tokio::io::Result<()> {
            let mut str = String::new();
            if let Some(pipe) = pipe.as_mut() {
                loop {
                    match pipe.read_u8().await {
                        Ok(c) => {
                            if c == 0x0a {
                                opt_sender
                                    .send(Some(str.clone()))
                                    .await
                                    .expect("send string line error");
                                str.clear();
                            } else if c == 0x3a && str.starts_with("Enter password") {
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
            }
            Ok(())
        }
        read_and_send(child.stdout.take(), opt_sender).await
    }
}
