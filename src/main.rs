use tokio::{sync::mpsc, try_join};
use z7::{Pushment, Z7};

use crate::nvim::Nvim;
mod nvim;
mod z7;

#[tokio::main]
async fn main() {
    log4rs::init_file("config/log4rs.yaml", Default::default()).unwrap();

    let (doc_sender, doc_recv) = mpsc::channel::<Pushment>(1);
    let (pwd_sender, pwd_recv) = mpsc::channel::<Option<String>>(1);
    let mut z7 = Z7::new(doc_sender);
    let _ = try_join!(z7.start(pwd_recv), Nvim::start(doc_recv, pwd_sender));
}
