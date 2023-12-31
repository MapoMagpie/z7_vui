use std::{
    fmt::Debug,
    io::{stdout, ErrorKind},
    path::Path,
    time::Duration,
};

use async_trait::async_trait;
use log::{error, info};
use nvim_rs::{
    compat::tokio::Compat, create::tokio::new_path, error::CallError, Handler, Neovim, Value,
};
use parity_tokio_ipc::Connection;
use tokio::{io::WriteHalf, process::Command, sync::mpsc, time::sleep, try_join};

use crate::z7::{Operation, Pushment};

// const OUTPUT_FILE: &str = "handler_drop.txt";
const NVIMPATH: &str = "nvim";

pub struct BufLineChanges {
    line_start: u64,
    line_end: u64,
    buf_id: i64,
    content: Vec<String>,
}

impl From<Vec<Value>> for BufLineChanges {
    fn from(args: Vec<Value>) -> Self {
        let line_start = args[2].as_u64().unwrap();
        let line_end = args[3].as_u64().unwrap();
        let buf_id = args[1].as_i64().unwrap();
        let content = args[4].as_array().unwrap();
        let mut content_vec = Vec::new();
        content.iter().for_each(|v| {
            content_vec.push(v.as_str().unwrap().to_string());
        });
        Self {
            line_start,
            line_end,
            buf_id,
            content: content_vec,
        }
    }
}

impl Debug for BufLineChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "(buf_id: {}, line_start: {}, line_end: {}, content: {:?})",
            self.buf_id, self.line_start, self.line_end, self.content
        )
    }
}

#[derive(Clone)]
struct NeovimHandler {
    oper_sender: mpsc::Sender<Operation>,
}

impl NeovimHandler {
    pub fn new(oper_sender: mpsc::Sender<Operation>) -> Self {
        Self { oper_sender }
    }
}

struct CursorAt {
    col: i64,
    #[allow(dead_code)]
    row: i64,
}

// [Array([String(Utf8String { s: Ok("n") }), Array([Integer(PosInt(1)), Integer(PosInt(0))])])]
impl From<Vec<Value>> for CursorAt {
    fn from(args: Vec<Value>) -> Self {
        let args = args[0].as_array().unwrap()[1].as_array().unwrap();
        let col = args[0].as_i64().unwrap();
        let row = args[1].as_i64().unwrap();
        Self { col, row }
    }
}

#[async_trait]
impl Handler for NeovimHandler {
    // type Writer = Compat<WriteHalf<Connection>>;
    type Writer = Compat<WriteHalf<Connection>>;

    async fn handle_notify(&self, name: String, args: Vec<Value>, nvim: Neovim<Self::Writer>) {
        match name.as_str() {
            "nvim_buf_lines_event" => {
                // info!("handle_notify: name: {}, args: {:?}", name, args);
                let buf_line = BufLineChanges::from(args);
                if buf_line.content.len() == 1 && buf_line.content[0] == "Enter password: " {
                    let _ = self.oper_sender.try_send(Operation::Retry);
                }
            }
            "nvim_insert_leave_event" => {
                // info!("handle_notify: name: {}, args: {:?}", name, args);
                // find password from buf line, then send password to 7z
                let buf = nvim.get_current_buf().await.expect("get current buf error");
                let cursor = CursorAt::from(args);
                let lines = buf
                    .get_lines((cursor.col - 1).max(0), cursor.col + 1, false)
                    .await
                    .expect("get lines error");
                for line in lines.into_iter() {
                    if line.starts_with("Enter password: ") {
                        let pwd = line.clone();
                        let pwd = pwd.trim_start_matches("Enter password:").trim().to_string();
                        if !pwd.is_empty() {
                            let _ = self.oper_sender.try_send(Operation::Password(pwd));
                        }
                        break;
                    }
                    if line.starts_with("Extract to: ") {
                        let path = line.clone();
                        let path = path.trim_start_matches("Extract to: ").trim().to_string();
                        if !path.is_empty() {
                            let _ = self.oper_sender.try_send(Operation::ExtractTo(path));
                        }
                        break;
                    }
                }
            }
            "nvim_execute_event" => {
                // info!("handle_notify: name: {}, args: {:?}", name, args);
                let _ = self.oper_sender.try_send(Operation::Execute);
            }
            "nvim_select_password_event" => {
                info!("handle_notify: name: {}, args: {:?}", name, args);
                let pwd = args[0].as_str();
                if let Some(pwd) = pwd {
                    let _ = self
                        .oper_sender
                        .try_send(Operation::SelectPassword(pwd.to_string()));
                }
            }
            "nvim_retry_event" => {
                // info!("handle_notify: name: {}, args: {:?}", name, args);
                let _ = self.oper_sender.try_send(Operation::Retry);
            }
            _ => {
                info!("handle_notify: name: {}, args: {:?}", name, args);
            }
        }
    }
}

const HIGHLIGHT_ERROR_GROUP: &str = "DiagnosticError";

pub struct Nvim;

impl Nvim {
    pub async fn start(
        mut doc_recv: mpsc::Receiver<Pushment>,
        oper_sender: mpsc::Sender<Operation>,
    ) -> tokio::io::Result<()> {
        if let Err(e) = Command::new(NVIMPATH)
            .args(["-u", "NONE", "--listen", "/tmp/nvim-socket-001"])
            .stdout(stdout())
            .spawn()
        {
            error!("Failed to start nvim: {}", e);
            return Err(e)?;
        }
        let path = Path::new("/tmp/nvim-socket-001");
        // wait for /tmp/nvim-socket-001 to be created
        while !path.exists() {
            sleep(Duration::from_millis(10)).await;
        }

        // clone oper_sender to NeovimHandler, it will drop when nvim quit, i want keep it alive;
        let oper_sender_ = oper_sender.clone();
        let handler = NeovimHandler::new(oper_sender_);
        let (nvim, io_handle) = new_path(path, handler)
            .await
            .expect("connect to nvim failed");

        Self::initialize_nvim(&nvim)
            .await
            .expect("initialize nvim error");

        // attach buf to subscribe "nvim_buf_lines_event"
        let curbuf = nvim.get_current_buf().await.expect("get current buf error");
        curbuf
            .attach(false, vec![])
            .await
            .expect("attach buf error");

        // receive pushment from 7z, then push to nvim
        let wait_push = async move {
            while let Some(pushment) = doc_recv.recv().await {
                match pushment {
                    Pushment::Full(lines, cursor) => {
                        // info!("recv pushment: {:?}", lines);
                        let err_line = lines
                            .iter()
                            .enumerate()
                            .find(|l| l.1.starts_with("ERROR:"))
                            .map(|(col, _)| col);
                        let line_count = curbuf.line_count().await.expect("get line count error");
                        let _ = curbuf.set_lines(0, line_count, false, lines).await;
                        if let Some(err_col) = err_line {
                            curbuf
                                .add_highlight(-1, HIGHLIGHT_ERROR_GROUP, err_col as i64, 0, -1)
                                .await
                                .expect("add highlight error");
                        }
                        if let Some((col, row)) = cursor {
                            let win = nvim.get_current_win().await.expect("get current win error");
                            win.set_cursor((col as i64, row as i64))
                                .await
                                .expect("set cursor error");
                            let _ = nvim
                                .call("nvim_command", vec!["startinsert!".into()])
                                .await
                                .expect("start insert error");
                        }
                    }
                    Pushment::Line(line, content) => curbuf
                        .set_lines(line as i64, line as i64, false, vec![content])
                        .await
                        .expect("set lines error"),
                    Pushment::None => {
                        nvim.quit_no_save().await.expect("quit nvim error");
                    }
                }
            }
            info!("pushment recv closed");
            tokio::io::Result::<()>::Err(ErrorKind::Other.into())
        };

        let wait_io = async move {
            match io_handle.await {
                Ok(Ok(())) => {
                    info!("everything ok!");
                }
                Ok(Err(e)) => {
                    error!("loop error: {:?}", e);
                }
                Err(e) => {
                    error!("join error: {:?}", e);
                }
            }
            // return error, then other task will be canceled
            tokio::io::Result::<()>::Err(ErrorKind::Other.into())
        };

        let _ = try_join!(wait_push, wait_io);
        info!("nvim quit");
        Ok(())
    }

    async fn initialize_nvim(
        nvim: &Neovim<Compat<WriteHalf<Connection>>>,
    ) -> Result<(), Box<CallError>> {
        // register "nvim_insert_leave_event", then subscribe it
        // nvim_insert_leave_event has been triggered, then check password from buf line, then send password to 7z
        nvim.create_autocmd(
            Value::Array(vec!["InsertLeave".into()]),
            vec![(
                "command".into(),
                Value::String(
                    r#"call rpcnotify(0, "nvim_insert_leave_event", [mode(), nvim_win_get_cursor(0)])"#.into(),
                ),
            )],
        )
        .await?;
        nvim.subscribe("nvim_insert_leave_event").await?;

        // register keymap "<space>c" to nvim, then nvim will notify "nvim_execute_event" to handler
        nvim.set_keymap(
            "n",
            "<space>c",
            r#":call rpcnotify(0, "nvim_execute_event")<CR>"#,
            vec![("silent".into(), true.into())],
        )
        .await?;
        nvim.subscribe("nvim_execute_event").await?;

        // register keymap "<space>r" to nvim, then nvim will notify "nvim_retry_event" to handler
        nvim.set_keymap(
            "n",
            "<space>r",
            r#":call rpcnotify(0, "nvim_retry_event")<CR>"#,
            vec![("silent".into(), true.into())],
        )
        .await?;
        nvim.subscribe("nvim_retry_event").await?;

        // register keymap "<space>q" to nvim, then nvim will quit
        nvim.set_keymap(
            "n",
            "<space>q",
            r#":qa!<CR>"#,
            vec![("silent".into(), true.into())],
        )
        .await?;

        // register keymap "<space>x" to nvim
        nvim.set_keymap(
            "n",
            "<space>x",
            r#"yi]:call rpcnotify(0, "nvim_select_password_event", getreg(0))<CR>"#,
            vec![("silent".into(), true.into())],
        )
        .await?;
        nvim.subscribe("nvim_select_password_event").await?;
        Ok(())
    }
}
