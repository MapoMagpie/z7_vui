use nvim::Nvim;
mod nvim;
mod z7;

#[tokio::main]
async fn main() {
    log4rs::init_file("config/log4rs.yaml", Default::default()).unwrap();
    let _ = Nvim::start().await;
    // let z7 = Z7::new();
    // let _ = z7.execute_list("test.7z").await;
    // let _ = try_join!(z7.execute_list("test.7z"), Nvim::start());
}
