use std::io::Write;
use net;
use net::{Room, User, ToPackage};
use tokio::net::{TcpSocket, TcpStream};

const SERVER_ADDR: &str = "127.0.0.1:5566";

#[tokio::main]
async fn main() {
    // let server_sock = TcpSocket::new_v4().unwrap();
    // server_sock.set_reuseaddr(true).unwrap();
    // server_sock.connect(SERVER_ADDR.parse().unwrap()).await.unwrap();
    let mut server_sock = TcpStream::connect(SERVER_ADDR).await.unwrap();
    println!("已连接服务器。");
    // 登录
    loop {
        let u = get_user_info();
        net::write(&mut server_sock, u.package().unwrap().as_slice()).await.unwrap();
        let stat = net::read(&mut server_sock).await.unwrap();
        let stat = std::str::from_utf8(&stat).unwrap();
        if stat.contains("OK") {
            break;
        }
        println!("请输入正确的用户！");
    }
    // 发送房间信息
    let room = get_room_info(&mut server_sock).await;
    println!("进入房间：{:?}", &room);

    loop {
        println!("{:#?}", serde_json::from_slice::<net::ClientInfo>(&net::read(&mut server_sock).await.unwrap()));
    }
}

fn get_user_info() -> User {
    print!("请输入用户名：");
    std::io::stdout().flush().unwrap();
    let mut u: User = User::default();
    std::io::stdin().read_line(&mut u.name).unwrap();
    u.name = String::from(u.name.trim());
    print!("请输入密码：");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_line(&mut u.passwd).unwrap();
    u.passwd = String::from(u.passwd.trim());
    u
}

async fn get_room_info(serv: &mut TcpStream) -> Room {
    let mut rom = Room { ..Default::default() };
    loop {
        rom.name = cin("请输入房间名：");
        rom.passwd = cin("请输入密码：");
        net::write(serv, rom.package().unwrap().as_slice()).await.unwrap();
        let buf = String::from_utf8(net::read(serv).await.unwrap()).unwrap();
        if buf.to_uppercase().contains("OK") {
            rom = net::read(serv).await.unwrap().into();
            return rom;
        }
    }
}

fn cin(msg: &str) -> String {
    print!("{}", msg);
    std::io::stdout().flush().unwrap();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).unwrap();
    buf.trim().to_string()
}
