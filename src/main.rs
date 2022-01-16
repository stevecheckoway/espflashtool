use espflashtool::Connection;

fn main() {
    println!("Hello, world! {:?}", Connection::new("").is_ok());
}
