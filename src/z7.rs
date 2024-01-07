use std::{
    ffi::OsStr,
    io::ErrorKind,
    path::PathBuf,
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
    ExtractTo(String),
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
    file: String,
    extract_to_path: Arc<RwLock<PathBuf>>,
    password_history_file: String,
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
            file: self.file.clone(),
            extract_to_path: self.extract_to_path.clone(),
            password_history_file: self.password_history_file.clone(),
        }
    }
}

impl Z7 {
    pub fn new(pusher: mpsc::Sender<Pushment>, opt: &Options) -> Self {
        let file = opt.file.file.clone();
        let extract_to_path = PathBuf::from(PathBuf::from(&file).parent().unwrap());
        let password_history_file = opt.password_history_file.clone();
        Self {
            document: Arc::new(RwLock::new(Document::new())),
            doc_sender: pusher,
            password: Arc::new(RwLock::new(None)),
            selected_password: Arc::new(RwLock::new(None)),
            stdin_pipe: Arc::new(RwLock::new(None)),
            execute_status: Arc::new(RwLock::new(ExecuteStatus::None)),
            file,
            extract_to_path: Arc::new(RwLock::new(extract_to_path)),
            password_history_file,
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
                Operation::ExtractTo(path) => {
                    self.set_extract_to_path(&path).await;
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

    async fn set_extract_to_path(&mut self, path: &str) {
        let mut extract_to_path = self.extract_to_path.write().await;
        *extract_to_path = PathBuf::from(path);
        let input = format!("Extract to: {}", extract_to_path.to_str().unwrap());
        let mut doc = self.document.write().await;
        doc.input(&input);
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
            {
                let mut status = self.execute_status.write().await;
                *status = ExecuteStatus::Pedding;
            }
            let password = {
                let password = self.password.read().await;
                password.clone()
            };
            let (exit_status, cmd) = match cmd {
                Cmd::List => {
                    {
                        let mut doc = self.document.write().await;
                        doc.layout_list();
                        doc.input(format!("Extract file: {}", self.file).as_str());
                        let extract_to_path = self.extract_to_path.read().await;
                        doc.input(
                            format!("Extract to: {}", extract_to_path.to_str().unwrap()).as_str(),
                        );
                    }
                    (
                        execute_list(&self.file, opt_sender, stdin_pipe, password).await?,
                        Cmd::List,
                    )
                }
                Cmd::Extract => {
                    {
                        let mut doc = self.document.write().await;
                        doc.layout_extract();
                    }
                    let extract_to_path = {
                        let extract_to_path = self.extract_to_path.read().await;
                        extract_to_path.to_str().unwrap().to_string()
                    };
                    (
                        execute_extract(
                            &self.file,
                            opt_sender,
                            stdin_pipe,
                            password,
                            &extract_to_path,
                        )
                        .await?,
                        Cmd::Extract,
                    )
                }
            };
            {
                let mut status = self.execute_status.write().await;
                if exit_status.success() {
                    *status = ExecuteStatus::None;
                    let mut doc = self.document.write().await;
                    if let Some(pwd) = self.password.read().await.clone() {
                        doc.input(format!("Save password: {}", pwd).as_str());
                    }
                    match cmd {
                        Cmd::List if check_same_directory(&doc.files()).is_none() => {
                            let filename = PathBuf::from(&self.file);
                            let filename = filename.file_stem().unwrap();
                            let mut extract_to_path = self.extract_to_path.write().await;
                            extract_to_path.push(filename);
                            let input =
                                format!("Extract to: {}", extract_to_path.to_str().unwrap());
                            doc.input(&input);
                            self.doc_sender
                                .send(Pushment::Line(4, input))
                                .await
                                .expect("send string line error");
                        }
                        _ => {}
                    }
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
                                format!("Password history file: {}", self.password_history_file)
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
    password: Option<String>,
) -> tokio::io::Result<ExitStatus> {
    let mut args = vec!["l", filename];
    let pwd = password.map(|s| format!("-p{}", s));
    if let Some(w) = pwd.as_ref() {
        args.push(w);
    }
    execute_cmd(opt_sender, stdin_pipe, args).await
}

async fn execute_extract(
    filename: &str,
    opt_sender: mpsc::Sender<Option<(String, usize)>>,
    stdin_pipe: Arc<RwLock<Option<ChildStdin>>>,
    password: Option<String>,
    extract_to_path: &str,
) -> tokio::io::Result<ExitStatus> {
    let out = format!("-o{}", extract_to_path);
    let mut args = vec!["x", filename, "-y", &out];
    let pwd = password.map(|s| format!("-p{}", s));
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

pub fn check_same_directory(files: &[String]) -> Option<String> {
    let mut prefix = String::new();
    let mut iter = files.iter();
    if let Some(first) = iter.next() {
        prefix.push_str(first);
        prefix.push('/');
        for file in iter {
            let mut i = 0;
            for (a, b) in prefix
                .chars()
                .zip(file.chars().chain(std::iter::repeat('/')))
            {
                if a != b {
                    break;
                } else if a == '/' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            prefix.truncate(i);
        }
    }
    if prefix.is_empty() || !prefix.ends_with('/') {
        None
    } else {
        Some(prefix)
    }
}

#[cfg(test)]
mod test {
    use super::check_same_directory;

    #[test]
    fn test_path_parent() {
        let path = std::path::PathBuf::from("/home/chen/code/vui-7z/src");
        let parent = path.parent().unwrap();
        assert_eq!(parent, std::path::PathBuf::from("/home/chen/code/vui-7z"));
        let path = std::path::PathBuf::from("code/vui-7z/src");
        let parent = path.parent().unwrap();
        assert_eq!(parent, std::path::PathBuf::from("code/vui-7z"));
    }

    #[test]
    fn test_path_display() {
        let path = std::path::PathBuf::from("");
        assert_ne!(format!("{:?}", path), "");
        assert_eq!(path.to_str().unwrap(), "");
        let path = std::path::PathBuf::from("/home/chen/code/vui-7z/src");
        assert_ne!(format!("{:?}", path), "/home/chen/code/vui-7z/src");
        assert_eq!(path.to_str().unwrap(), "/home/chen/code/vui-7z/src");
    }

    #[test]
    fn test_check_same_prefix() {
        let files = ["test/03-e_03.png", "test/01-e_01.png"];
        let files = files.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let prefix = check_same_directory(&files);
        assert_eq!(prefix, Some("test/".to_string()));

        let files = ["03-e_03.png", "01-e_01.png"];
        let files = files.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let prefix = check_same_directory(&files);
        assert_eq!(prefix, None);

        let files = ["test", "test/01-e_01.png"];
        let files = files.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let prefix = check_same_directory(&files);
        assert_eq!(prefix, Some("test/".to_string()));

        let files = ["test2/01-e_01.png", "test/01-e_01.png"];
        let files = files.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let prefix = check_same_directory(&files);
        assert_eq!(prefix, None);

        let files = ["test", "test2"];
        let files = files.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let prefix = check_same_directory(&files);
        assert_eq!(prefix, None);
    }
}
