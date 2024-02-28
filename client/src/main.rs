use std::{
    env, io::Write, mem::size_of, net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc, time::Duration
};
use env_logger::Builder;
use log::{debug, error, info, warn, LevelFilter};
use net::{self, BaseUserInfo, Room, ToPackage, TryRead, User, ClientInfo};
use rand::Rng;
use tokio::{
    io::Result,
    net::{TcpSocket, TcpStream},
    sync::{mpsc::{self, Sender, Receiver}, watch, Mutex},
    time::sleep
};
// 默认服务器地址
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:5566";

#[tokio::main]
async fn main() {
    // 在windows下默认不是utf-8，将终端设置为utf-8
    #[cfg(windows)]
    {   extern "C" {
            fn system(cmd: *const std::ffi::c_char) -> std::ffi::c_int;
        }
        unsafe { system("chcp 65001\0".as_ptr() as *const std::ffi::c_char); }
    }
    let (msg_tx, msg_rx) = mpsc::channel::<Msg>(128);
    let (cin_tx, mut cin_rx) = watch::channel(String::new());
    let msg_handle = tokio::spawn(msg_handle(msg_rx));
    let (log_tx, log_rx) = mpsc::channel::<String>(64);
    let log_handle = tokio::spawn(log_handle(log_rx, msg_tx.clone()));
    // 设置日志输出格式
    Builder::new()
        .format(|buf, record| {
            let color = match record.level() {
                log::Level::Trace => "",
                log::Level::Debug => "\x1B[32m",
                log::Level::Info => "\x1B[32m",
                log::Level::Warn => "\x1B[35m",
                log::Level::Error => "\x1B[1;31m",
            };
            writeln!(buf,
                "{}[{} {}] {}\x1B[0m",
                color,
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.args()
            )
        })
        .filter(None, LevelFilter::Info)
        .target(env_logger::Target::Pipe(Box::new(MyLogTarget::new(log_tx))))
        .init();
    let mut server_addr: String = DEFAULT_SERVER_ADDR.into();
    if env::args().len() > 1 {
        server_addr = env::args().nth(1).unwrap();
    }
    let loc_addr = {
        let mut rng = rand::thread_rng();
        let port = rng.gen_range(4000..9000);
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), port))
    };
    let mut server_stream = {
        let server_sock = TcpSocket::new_v4().unwrap();
        #[cfg(target_family = "unix")]
        {server_sock.set_reuseport(true).unwrap();}
        server_sock.set_reuseaddr(true).unwrap();
        server_sock.bind(loc_addr).unwrap();
        match server_sock.connect(server_addr.parse().unwrap()).await {
            Ok(sock) => { sock },
            Err(e) => {
                eprintln!("无法连接到服务器。{}", e);
                return;
            },
        }
    };
    info!("已连接服务器。");
    let msg_tx_clone = msg_tx.clone();
    let clients: Arc<Mutex<Vec<PeerInfo>>> = Arc::new(Mutex::new(Vec::new()));
    let clients_ = clients.clone();
    let (shh_tx, ssh_rx) = tokio::sync::oneshot::channel();
    let (sh_tx, sh_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        // 登录
        let user_info = login(&mut server_stream, &msg_tx_clone, &mut cin_rx).await;
        // 在克隆前先将内容清空
        cin_rx.borrow_and_update();
        // 发送房间信息
        let room = join_room(&mut server_stream, &msg_tx_clone, &mut cin_rx).await;
        info!("进入房间：{:?}", &room);

        cin_rx.borrow_and_update();
        init_room(&mut server_stream, &user_info, &mut cin_rx, &msg_tx_clone).await;

        let server_handle = tokio::spawn(handle_server(
            server_stream, user_info, msg_tx_clone, clients_, cin_rx, sh_rx
        ));
        shh_tx.send(server_handle).unwrap();
    });
    // 主线程来监控标准输入
    poll_user_input(&cin_tx, &msg_tx).await;
    let server_handle = ssh_rx.await.unwrap();
    info!("正在等待所有任务结束");
    if let Err(e) = sh_tx.send(true) {
        error!("Server handle tx Send fail, {}", e);
    };
    for _ in 0..50 {
        let _ = msg_tx.send(Msg::Other(".".to_string())).await;
        let _ = cin_tx.send('\x03'.to_string());
        let mut finish = true;
        if !server_handle.is_finished() {
            finish = false;
        } else {
            for peer in clients.lock().await.iter_mut() {
                if !peer.handle.is_finished() {
                    finish = false;
                    break;
                }
            }
        }
        if finish {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    for peer in clients.lock().await.iter_mut() {
        peer.handle.abort();
    }
    server_handle.abort();
    drop(msg_tx);
    log_handle.abort();
    tokio::try_join!(msg_handle).unwrap();
}

async fn poll_user_input(cin_tx: &watch::Sender<String>, msg_tx: &mpsc::Sender<Msg>) {
    let getter = getch::Getch::new();
    let mut str_buf = String::new();
    let mut ch_buf = [0u8; size_of::<char>()];
    let mut ch_buf_len = 0;
    loop {
        let c = match getter.getch() {
            Ok(c) => { c },
            Err(_) => {continue;},
        };
        if ch_buf_len == 0 {
            if c == 3 {
                msg_tx.send(Msg::Stdin(c as char)).await.unwrap();
                break;
            }
        }
        ch_buf[ch_buf_len] = c;
        ch_buf_len = (ch_buf_len + 1) % size_of::<char>();
        if let Ok(c) = std::str::from_utf8(&ch_buf) {
            let c = if let Some(c) = c.chars().nth(0) { c } else { continue; };
            debug!("stdin char: {:?}", c);
            msg_tx.send(Msg::Stdin(c)).await.unwrap();
            ch_buf_len = 0;
            ch_buf = [0u8; size_of::<char>()];
            match c {
                '\x0D' | '\n' => {
                    let sin = str_buf.trim().to_string();
                    if sin.len() != 0 {
                        if sin.chars().nth(0).unwrap() == ':' {
                            warn!("指令功能待开发，请稍后...");
                        }
                        else if let Err(e) = cin_tx.send(sin) {
                            error!("cin tx send error!:{}", e);
                            break;
                        }
                    }
                    str_buf.clear();
                },
                '\x08' | '\x7F' => {
                    str_buf.pop();
                },
                _ => {
                    str_buf.push(c);
                }
            }
        }
    }
}

async fn init_room(mut server_stream: &mut TcpStream, user_info: &User,
    cin_rx: &mut watch::Receiver<String>, msg_tx: &Sender<Msg>
) {
    // 接收服务端发送过来的所有房间内的peer
    let clients: Vec<ClientInfo> = serde_json::from_slice(&net::read(&mut server_stream).await.unwrap()).unwrap();
    info!("房间中共有{}个人", clients.len());
    if clients.len() > 0 { info!("开始建立连接..."); }
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
                    warn!("连接{:?}失败", &ci);
                    return Err(ci);
                }
            };
            let other = {
                if let Ok(ci) = swap_info(&user_info, &mut stm,ci.addr).await {
                    ci
                }
                else {
                    warn!("连接{:?}失败，无法验证身份", &ci);
                    return Err(ci);
                }
            };
            info!("Connect: {:?}", &other);
            tokio::spawn(Peer::new(
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
    info!("Connent Room Done.");
}

async fn handle_server(mut server_stream: TcpStream, user_info: User,
        msg_tx: Sender<Msg>, clients: Arc<Mutex<Vec<PeerInfo>>>, cin_rx: watch::Receiver<String>,
        mut sh_rx: tokio::sync::oneshot::Receiver<bool>
) {
    let addr = server_stream.local_addr().unwrap();
    let mut reader = TryRead::new();
    'a: loop {
        tokio::select! {
            _ = sleep(Duration::from_millis(5000)) => {
                debug!("server 发送心跳包");
                net::write(&mut server_stream, "".as_bytes()).await.unwrap();
            },
            res = server_stream.readable() => {
                if let Err(e) = res {
                    error!("Fail to server_stream.readable() {}", e);
                    break;
                }
                debug!("server readable");
                let pkg = loop {
                    match reader.poll(&mut server_stream) {
                        Ok(_) => {
                            break reader.package();
                        },
                        Err(e) => {
                            if let Some(e) = e.can_continue() {
                                warn!("{:?}", e);
                                break 'a;
                            } else {
                                continue 'a;
                            }
                        },
                    }
                };
                debug!("server read pkg done.");
                if pkg.len() == 0 {
                    // 心跳包，不用管
                    debug!("from server 心跳包");
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
                        error!("Fail to bind {} {}", &addr, e);
                        continue;
                    };
                    let mut sock = {
                        match sock.connect(ci.addr.clone()).await {
                            Ok(s) => s,
                            Err(e) => {
                                warn!("Fail to connent {:?} : {}", theci, e);
                                continue;
                            }
                        }
                    };
                    if let Err(e) = swap_info(&user_info, &mut sock, ci.addr.clone()).await {
                        warn!("无法获取客户端信息{:?}: {}", theci, e.to_string());
                    }
                    let prcs = Peer::new(&theci, sock, msg_tx.clone(), cin_rx.clone());
                    let handle = tokio::spawn(prcs.poll());
                    info!("Connect: {:?}", &theci);
                    clients.lock().await.push(PeerInfo {
                        ci: theci,
                        handle
                    });
                } else {
                    info!("Unknown Pakage {:?}", &pkg);
                }
            },
            _ = &mut sh_rx => {
                break;
            }
        }
    }
    info!("Server disconnent.");
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
                &match net::read(&mut sock).await {
                    Ok(pkg) => { pkg },
                    Err(e) => { return match e {
                        net::ErrorType::IO(e) => { Err(e) }
                        _ => { Err(std::io::ErrorKind::Other.into()) },
                    }; },
                })?;
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

async fn msg_handle(mut msg_rx: Receiver<Msg>) {
    let mut in_buf = String::new();
    let mut other_buf = String::new();
    loop {
        let res = msg_rx.recv().await;
        if let None = res {
            break;
        }
        let msg = res.unwrap();
        match msg {
            Msg::Log(log) => {
                print!("\x1B[1G\x1B[2K{}", log);
                print!("{}{}", other_buf, in_buf);
            },
            Msg::UserMsg(msg) => {
                print!("\x1B[1G\x1B[2K[{}] {}: {}\n",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), &msg.0.name, &msg.1);
                print!("{}{}", other_buf, in_buf);
            },
            Msg::Stdin(ch) => {
                match ch {
                    '\x0D' | '\n' => {
                        if in_buf.len() != 0 {
                            in_buf.clear();
                            other_buf.clear();
                            println!();
                        }
                    },
                    '\x08' | '\x7F' => {
                        if let Some(_) = in_buf.pop() {
                            print!("\x1B[1G\x1B[2K{}{}", other_buf, in_buf);
                        }
                    },
                    _ => {
                        in_buf.push(ch);
                        print!("{}", ch);
                    }
                }
            },
            Msg::Other(str) => {
                other_buf.push_str(&str);
                other_buf = other_buf.split('\n').last().unwrap().to_string();
                print!("\x1B[1G\x1B[2K{}{}", other_buf, in_buf);
            },
        }
        let _ = std::io::stdout().flush();
    }
}

async fn log_handle(mut log_rx: Receiver<String>, msg_tx: Sender<Msg>) {
    loop {
        if let Some(log) = log_rx.recv().await {
            msg_tx.send(Msg::Log(log)).await.unwrap();
        }
    }
}

#[derive(Debug)]
enum Msg {
    UserMsg((BaseUserInfo, String)),
    Log(String),
    Stdin(char),
    Other(String),
}

struct PeerInfo {
    ci: ClientInfo,
    handle: tokio::task::JoinHandle<()>,
}

struct Peer {
    ci: ClientInfo,
    sock: TcpStream,
    msg_tx: Sender<Msg>,
    cin_rx: watch::Receiver<String>,
}

impl Peer {
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
                            Ok(_) => {
                                let pkg = reader.package();
                                let msg = String::from_utf8(pkg);
                                if let Ok(msg) = msg {
                                    // 判断是否为心跳包，是就不用管
                                    if msg.len() == 0 { continue; }
                                    self.msg_tx.send(Msg::UserMsg((bui.clone(), msg))).await.unwrap();
                                }
                            },
                            Err(e) => {
                                if let Some(e) = e.can_continue() {
                                    warn!("{:?}", e);
                                    break 'a;
                                } else {
                                    break;
                                }
                            },
                        }
                    }
                },
                cres = self.cin_rx.changed() => {
                    if let Err(_) = cres {
                        break;
                    }
                    let msg = self.cin_rx.borrow_and_update().clone();
                    if let Some(ch) = msg.chars().nth(0) {
                        if ch == (3 as char) {
                            break;
                        }
                    }
                    net::write(&mut self.sock, msg.as_bytes()).await.unwrap();
                },
                // 每隔一段时间确认一次客户端是否存在
                _ = sleep(Duration::from_secs(1 * 60)) => {
                    if let Err(_) = net::write(&mut self.sock, "".as_bytes()).await {
                        break;
                    };
                },
            };
        }
        info!("Disconnect: {:?}", bui);
        drop(self.msg_tx);
    }
}

async fn login(mut serv: &mut TcpStream, msg_tx: &mpsc::Sender<Msg>, cin_rx: &mut watch::Receiver<String>) -> User {
    let mut cin = Cin {msg_tx, cin_rx};
    loop {
        let mut u = User {
            id: 0,
            name: cin.get("请输入用户名：").await,
            passwd: cin.get("请输入密码：").await,
        };
        net::write(&mut serv, u.package().unwrap().as_slice()).await.unwrap();
        let stat = net::read(&mut serv).await.unwrap();
        let stat = std::str::from_utf8(&stat).unwrap();
        if stat.contains("OK") {
            let base_info: net::BaseUserInfo = {
                let pkg = net::read(serv).await.unwrap();
                if let Ok(user_info) = serde_json::from_slice(&pkg) {
                    user_info
                } else {
                    continue;
                }
            };
            u.id = base_info.id;
            info!("登录成功, ID: {}", u.id);
            break u;
        }
        warn!("请输入正确的用户！");
    }
}

async fn join_room(serv: &mut TcpStream, msg_tx: &mpsc::Sender<Msg>, cin_rx: &mut watch::Receiver<String>) -> Room {
    let mut rom = Room { ..Default::default() };
    let mut cin = Cin {msg_tx, cin_rx};
    loop {
        rom.name = cin.get("请输入房间名：").await;
        rom.passwd = cin.get("请输入密码：").await;
        net::write(serv, rom.package().unwrap().as_slice()).await.unwrap();
        let buf = String::from_utf8(net::read(serv).await.unwrap()).unwrap();
        if buf.to_uppercase().contains("OK") {
            rom = net::read(serv).await.unwrap().into();
            return rom;
        }
        warn!("请确认房间信息是否正确！");
    }
}

struct Cin<'a> {
    msg_tx: &'a mpsc::Sender<Msg>,
    cin_rx: &'a mut watch::Receiver<String>
}

impl Cin<'_> {
    async fn get(&mut self, msg: &str) -> String {
        self.msg_tx.send(Msg::Other(msg.to_string())).await.unwrap();
        self.cin_rx.changed().await.unwrap();
        self.cin_rx.borrow_and_update().clone().trim().to_string()
    }
}

struct MyLogTarget {
    buf: String,
    tx: mpsc::Sender<String>,
}

impl MyLogTarget {
    fn new(tx: mpsc::Sender<String>) -> Self {
        Self {
            buf: String::new(), tx
        }
    }
}

impl std::io::Write for MyLogTarget {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if let Ok(str) = std::str::from_utf8(buf) {
            self.buf.push_str(str);
            if self.buf.contains('\n') {
                let tx = self.tx.clone();
                let buf = std::mem::take(&mut self.buf);
                tokio::spawn(async move {
                    if let Err(e) = tx.send(buf).await {
                        eprint!("{}", e);
                    }
                });
            }
        }
        Ok(0)
    }

    fn flush(&mut self) -> Result<()> {
        let tx = self.tx.clone();
        let buf = std::mem::take(&mut self.buf);
        tokio::spawn(async move {
            if let Err(e) = tx.send(buf).await {
                eprint!("{}", e);
            }
        });
        Ok(())
    }
}
