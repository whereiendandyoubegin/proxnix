use proxnix_pure::pure_only;

#[pure_only]
fn connect() -> bool {
    std::net::TcpStream::connect("127.0.0.1:80").is_ok()
}

fn main() {}
