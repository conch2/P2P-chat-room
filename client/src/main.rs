use std::{env, io::Write, net::{SocketAddr, SocketAddrV4, Ipv4Addr}, sync::Arc, time::Duration};
use net::{self, BaseUserInfo, Room, ToPackage, TryRead, User, ClientInfo};
use rand::Rng;
use tokio::{
    io::{stdin, AsyncBufReadExt, BufReader, Result},
    net::{TcpListener, TcpSocket, TcpStream},
    sync::{mpsc::{self, Sender, Receiver}, watch, Mutex},
    time::sleep
};

const SERVER_ADDR: &str = "127.0.0.1:5566";

#[tokio::main]
async fn main() {
    env_logger::init();
    let mut server_addr: String = SERVER_ADDR.into();
    if env::args().len() > 1 {
        server_addr = env::args().nth(1).unwrap();
    }
    let loc_addr = {
        let mut rng = rand::thread_rng();
        let port = rng.gen_range(4000..9000);
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), port))
    };
    let (listener, mut server_stream) = {
        let sock = TcpSocket::new_v4().unwrap();
        #[cfg(target_family = "unix")]
        {sock.set_reuseport(true).unwrap();}
        sock.set_reuseaddr(true).unwrap();
        println!("local address: {}", loc_addr);
        sock.bind(loc_addr).unwrap();
        let server_sock = TcpSocket::new_v4().unwrap();
        #[cfg(target_family = "unix")]
        {server_sock.set_reuseport(true).unwrap();}
        server_sock.set_reuseaddr(true).unwrap();
        server_sock.bind(loc_addr).unwrap();
        (sock.listen(1024).unwrap(), server_sock.connect(server_addr.parse().unwrap()).await.unwrap())
    };
    println!("已连接服务器。");
    // 登录
    let user_info = get_user_info(&mut server_stream).await;
    // 发送房间信息
    let room = get_room_info(&mut server_stream).await;
    println!("进入房间：{:?}", &room);
    let (msg_tx, msg_rx) = mpsc::channel::<Msg>(64);
    let (cin_tx, cin_rx) = watch::channel(String::new());

    init_room(&mut server_stream, &user_info, cin_rx.clone(), msg_tx.clone()).await;

    let clients: Arc<Mutex<Vec<ClientInfo>>> = Arc::new(Mutex::new(Vec::new()));
    let server_handle = tokio::spawn(server_handle(
        server_stream, user_info.clone(), msg_tx.clone(), clients.clone(), cin_rx.clone()
    ));
    let accecpt_handle = tokio::spawn(accecpt_handle(
        listener, user_info.clone(), msg_tx.clone(), cin_rx.clone()
    ));
    let msg_handle = tokio::spawn(msg_handle(msg_rx));
    let cin_handle = tokio::spawn(cin_handle(cin_tx));

    tokio::try_join!(server_handle, accecpt_handle, msg_handle, cin_handle).unwrap();
}

async fn init_room(mut server_stream: &mut TcpStream, user_info: &User,
    cin_rx: watch::Receiver<String>, msg_tx: Sender<Msg>
) {
    let clients: Vec<ClientInfo> = serde_json::from_slice(&net::read(&mut server_stream).await.unwrap()).unwrap();
    println!("房间中共有{}个人", clients.len());
    if clients.len() > 0 { println!("开始建立连接..."); }
    let addr = server_stream.local_addr().unwrap();
    let mut set = tokio::task::JoinSet::new();
    for ci in clients {
        let user_info = user_info.clone();
        let cin_rx = cin_rx.clone();
        let msg_tx = msg_tx.clone();
        set.spawn(async move {
            let mut stm = {
                let sock = TcpSocket::new_v4().unwrap();
                #[cfg(target_family = "unix")]
                {sock.set_reuseport(true).unwrap();}
                sock.set_reuseaddr(true).unwrap();
                sock.bind(addr.clone()).unwrap();
                if let Ok(stm) = sock.connect(ci.addr.clone()).await {
                    stm
                } else {
                    println!("连接{:?}失败", &ci);
                    return Err(ci);
                }
            };
            let other = {
                if let Ok(ci) = swap_info(&user_info, &mut stm,ci.addr).await {
                    ci
                }
                else {
                    println!("连接{:?}失败，无法验证身法", &ci);
                    return Err(ci);
                }
            };
            println!("Connect: {:?}", &other);
            tokio::spawn(Process::new(
                &other, stm, msg_tx, cin_rx
            ).poll());
            Ok(())
        });
    }
    let mut e_cis: Vec<ClientInfo> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(res) = res {
            if let Err(ci) = res {
                // 将未成功连接的回馈给服务端
                e_cis.push(ci);
            }
        }
    }
    net::write(&mut server_stream, &serde_json::to_vec(&e_cis).unwrap()).await.unwrap();
    println!("OK.");
}

async fn server_handle(mut server_stream: TcpStream, user_info: User,
        msg_tx: Sender<Msg>, clients: Arc<Mutex<Vec<ClientInfo>>>, cin_rx: watch::Receiver<String>
) {
    let addr = server_stream.local_addr().unwrap();
    loop {
        tokio::select! {
            _ = sleep(Duration::from_millis(5000)) => {
                net::write(&mut server_stream, "".as_bytes()).await.unwrap();
            },
            _ = server_stream.readable() => {
                let pkg = {
                    if let Ok(pkg) = net::read(&mut server_stream).await { pkg }
                    else { break; }
                };
                if pkg.len() == 0 {
                    continue;
                }
                if let Ok(ci) = serde_json::from_slice::<net::ClientInfo>(&pkg) {
                    let theci = ClientInfo {
                        id: ci.id,
                        name: ci.name,
                        addr: ci.addr.clone()
                    };
                    let sock = TcpSocket::new_v4().unwrap();
                    #[cfg(target_family = "unix")]
                    {sock.set_reuseport(true).unwrap();}
                    sock.set_reuseaddr(true).unwrap();
                    if let Err(e) = sock.bind(addr.clone()) {
                        println!("Fail to bind {} {}", &addr, e);
                        continue;
                    };
                    let mut sock = {
                        if let Ok(s) = sock.connect(ci.addr.clone()).await { s }
                        else { continue; }
                    };
                    if let Err(e) = swap_info(&user_info, &mut sock, ci.addr.clone()).await {
                        println!("无法获取客户端信息{:?}: {}", theci, e.to_string());
                    }
                    let prcs = Process::new(&theci, sock, msg_tx.clone(), cin_rx.clone());
                    tokio::spawn(prcs.poll());
                    println!("Connect: {:?}", &theci);
                    clients.lock().await.push(theci);
                } else {
                    println!("Unknown Pakage {:?}", &pkg);
                }
            },
        };
    }
}

/// 交换相互的信息
async fn swap_info(user_info: &User, mut sock: &mut TcpStream, addr: SocketAddr) -> Result<ClientInfo> {
    // 将自己的信息发送到连接的客户端
    let bui = net::BaseUserInfo {
        id: user_info.id,
        name: user_info.name.clone(),
    };
    net::write(&mut sock, &serde_json::to_vec(&bui).unwrap()).await?;
    // 接收传过来的信息
    let other = {
        let bui = serde_json::from_slice::<net::BaseUserInfo>(
                &net::read(&mut sock).await?)?;
        ClientInfo {
            id: bui.id,
            name: bui.name,
            addr: addr,
        }
    };
    // 这里就可以对传过来的信息和服务端的信息进行比对
    // TODO
    Ok(other)
}

async fn accecpt_handle(listener: TcpListener, user_info: User,
    msg_tx: Sender<Msg>, cin_rx: watch::Receiver<String>
) {
    loop {
        let (mut stm, addr) = listener.accept().await.unwrap();
        let other = {
            if let Ok(ci) = swap_info(&user_info, &mut stm, addr).await {
                ci
            }
            else { continue; }
        };
        println!("Accept: {:?}", &other);
        tokio::spawn(Process::new(
            &other, stm, msg_tx.clone(), cin_rx.clone()
        ).poll());
    }
}

async fn msg_handle(mut msg_rx: Receiver<Msg>) {
    loop {
        let res = msg_rx.recv().await;
        if let None = res {
            panic!("Fail to recv message");
        }
        let msg = res.unwrap();
        println!("{}: {}", &msg.from.name, &msg.msg);
    }
}

async fn cin_handle(cin_tx: watch::Sender<String>) {
    loop {
        let mut reader = BufReader::new(stdin());
        let mut buf = String::new();
        reader.read_line(&mut buf).await.unwrap();
        buf = buf.trim().to_string();
        if buf.len() != 0 {
            cin_tx.send(buf.clone()).unwrap();
        }
    }
}

#[derive(Debug)]
struct Msg {
    from: BaseUserInfo,
    msg: String,
}

struct Process {
    ci: ClientInfo,
    sock: TcpStream,
    msg_tx: Sender<Msg>,
    cin_rx: watch::Receiver<String>,
}

impl Process {
    fn new(ci: &ClientInfo, sock: TcpStream, msg_tx: Sender<Msg>, cin_rx: watch::Receiver<String>) -> Self {
        Self {
            ci: ClientInfo {
                id: ci.id,
                name: ci.name.clone(),
                addr: ci.addr.clone(),
            }, sock, msg_tx, cin_rx
        }
    }

    async fn poll(mut self) {
        let bui = BaseUserInfo{ id: self.ci.id, name: self.ci.name.clone() };
        let mut reader = TryRead::new();
        'a: loop {
            tokio::select! {
                rres = self.sock.readable() => {
                    if let Err(_) = rres {
                        return ;
                    }
                    loop {
                        match reader.poll(&mut self.sock) {
                            Ok(rlen) => {
                                if rlen == 0 {
                                    break 'a;
                                }
                                let pkg = reader.package();
                                let msg = String::from_utf8(pkg);
                                if let Ok(msg) = msg {
                                    let msg = Msg {
                                        from: bui.clone(),
                                        msg
                                    };
                                    self.msg_tx.send(msg).await.unwrap();
                                }
                            },
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => { break; },
                            Err(_) => { break 'a; }
                        }
                    }
                },
                cres = self.cin_rx.changed() => {
                    if let Err(_) = cres {
                        break;
                    }
                    let msg = self.cin_rx.borrow_and_update().clone();
                    let msg = Msg {
                        from: bui.clone(),
                        msg: msg.clone(),
                    };
                    net::write(&mut self.sock, msg.msg.as_bytes()).await.unwrap();
                },
            };
        }
        println!("Disconnect: {:?}", bui);
    }
}

async fn get_user_info(mut serv: &mut TcpStream) -> User {
    loop {
        let mut u = User {
            id: 0,
            name: cin("请输入用户名："),
            passwd: cin("请输入密码："),
        };
        net::write(&mut serv, u.package().unwrap().as_slice()).await.unwrap();
        let stat = net::read(&mut serv).await.unwrap();
        let stat = std::str::from_utf8(&stat).unwrap();
        if stat.contains("OK") {
            let base_info: net::BaseUserInfo = {
                let pkg = net::read(serv).await.unwrap();
                if let Ok(ui) = serde_json::from_slice(&pkg) {
                    ui
                } else {
                    continue;
                }
            };
            u.id = base_info.id;
            break u;
        }
        println!("请输入正确的用户！");
    }
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
