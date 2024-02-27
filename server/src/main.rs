use log::{debug, error, info, warn};
use tokio::net::{TcpListener, TcpStream};
use std::collections::HashMap;
use std::{env, process::exit, io::Write};
use std::{fmt::Debug, time::Duration};
use std::{net::SocketAddr, str, sync::Arc};
use net::*;
use tokio::{sync::*, io::*, time::sleep};

const LISTEN_ADDR: &str = "0.0.0.0:5566";

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
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
        .filter(None, log::LevelFilter::Debug)
        .target(env_logger::Target::Stdout)
        .init();
    // 默认端口5566
    let mut addr: String = LISTEN_ADDR.into();
    if env::args().len() > 1 {
        let mut args = env::args();
        let port = { args.nth(1).unwrap() };
        addr = format!("0.0.0.0:{}", port);
    }
    Server::new(&addr).await
        .run().await;
}

struct Server {
    addr: String,
    listener: TcpListener,
    rooms: Arc<Mutex<AllRoomInfo>>,
    users: Arc<Mutex<AllUserInfo>>,
}

impl Server {
    /// 初始化一个服务
    async fn new(addr: &str) -> Self {
        Server {
            addr: addr.into(),
            listener: {
                if let Ok(listener) = TcpListener::bind(addr).await {
                    listener
                } else {
                    error!("请检查该地址是否正确：{}", addr);
                    exit(1);
                }
            },
            rooms: Arc::new(Mutex::new(AllRoomInfo::new())),
            users: Arc::new(Mutex::new(AllUserInfo::default())),
        }
    }

    async fn run(self) {
        info!("server run in {}", self.addr);

        let t1 = tokio::spawn(Self::poll_cmd(self.rooms.clone(), self.users.clone()));
        let t2 = tokio::spawn(Self::accept(self.listener, self.rooms.clone(), self.users.clone()));
        tokio::join!(t1, t2).0.unwrap();
    }

    /// 处理新接入的客户端
    /// 创建一个任务处理
    async fn accept(listener: TcpListener,  rooms: Arc<Mutex<AllRoomInfo>>, users: Arc<Mutex<AllUserInfo>>) {
        loop {
            let (stm, addr) = listener.accept().await.unwrap();
            debug!("New peer: {}", addr);
            // 创建任务处理
            tokio::spawn(CertificationCenter::poll(stm, addr, rooms.clone(), users.clone()));
        }
    }

    /// 处理标准输入
    async fn poll_cmd(rooms: Arc<Mutex<AllRoomInfo>>, users: Arc<Mutex<AllUserInfo>>) {
        let mut reader = BufReader::new(stdin());
        loop {
            let mut buf = String::new();
            reader.read_line(&mut buf).await.unwrap();
            if buf.len() == 0 {
                warn!("stdin has been closed");
                break;
            }
            if String::from(buf.trim()).to_uppercase() == "echo rooms".to_uppercase() {
                let lock = rooms.lock().await;
                println!("{:#?}", &lock as &AllRoomInfo);
            }
            else if String::from(buf.trim()).to_uppercase() == "echo users".to_uppercase() {
                let lock = users.lock().await;
                println!("{:#?}", &lock as &AllUserInfo);
            }
            else if String::from(buf.trim()).to_uppercase() == "exit".to_uppercase() {
                exit(0);
            }
        }
    }
}

/// 注册中心，确保客户端成功登录
struct CertificationCenter;

impl CertificationCenter {
    async fn poll(mut stm: TcpStream, addr: SocketAddr,
            rooms: Arc<Mutex<AllRoomInfo>>, users: Arc<Mutex<AllUserInfo>>
        ) {
        let mut user = match Self::wait_login(&mut stm, users.clone()).await {
            Ok(u) => u,
            Err(e) => {
                warn!("客户端[{}]用户登录失败 {}", addr, e);
                return ;
            }
        };
        {
            let mut users = users.lock().await;
            users.insert(&mut user);
        }
        write(&mut stm, "OK".as_bytes()).await.unwrap();
        // 将用户信息反馈给客户端
        let base_info = BaseUserInfo {
            id: user.id,
            name: user.name.clone(),
        };
        write(&mut stm, &serde_json::to_vec(&base_info).unwrap()).await.unwrap();
        info!("{}: {:?}", &addr, &user);
        let uid = user.id;
        let mut prcs = Process::new(user, stm, addr, rooms.clone());
        prcs.poll().await.ok();
        {
            let mut users = users.lock().await;
            users.remove(uid);
            info!("{}: Quit {:?}", prcs.addr, prcs.user);
        } {
            // 退出
            let mut lock = rooms.lock().await;
            for rid in prcs.room.iter() {
                if let None = lock.by_id.get(&rid) {
                    continue;
                }
                // 获取删除自己后房间剩余的人数
                let len: usize = {
                    let rom = lock.by_id.get_mut(rid).unwrap();
                    rom.cs.remove(&prcs.user.id);
                    info!("User[id: {}, name: \"{}\"] remove from Room[id: {}, name: \"{}\"]",
                            base_info.id, base_info.name, rom.id, rom.name);
                    rom.cs.len()
                };
                // 如果房间为空了就删除房间
                if len == 0 {
                    let rom = lock.remove(*rid);
                    info!("{:?} was destroyed", rom);
                }
            }
        }
    }

    /// 等待用户登录
    /// 成功返回用户信息
    async fn wait_login(mut stm: &mut TcpStream, users: Arc<Mutex<AllUserInfo>>) -> Result<User> {
        loop {
            let pack = match read(stm).await {
                Ok(pkg) => { pkg },
                Err(e) => {
                    match e {
                        ErrorType::IO(e) => { return Err(e); },
                        ErrorType::MissingHead(head) => {
                            if head.len() == 0 {
                                break Err(std::io::ErrorKind::Other.into());
                            }
                            continue;
                        },
                        e => { debug!("{:?}", e); continue; },
                    }
                },
            };
            if pack.len() == 0 {
                continue;
            }
            if let Ok(u) = serde_json::from_slice::<User>(&pack) {
                let users = users.lock().await;
                if u.name.len() != 0 && u.passwd.len() != 0 {
                    // 账号已存在
                    if let Some(_) = users.by_name.get(&u.name) {
                        write(&mut stm, "User already exists".as_bytes()).await?;
                        continue;
                    } else {
                        // 返回用户信息
                        break Ok(u)
                    }
                }
            }
            write(&mut stm, "Fail to login user".as_bytes()).await?;
        }
    }
}

#[derive(Debug)]
struct AllRoomInfo {
    by_id: HashMap<u32, RoomFull>,
    by_name: HashMap<String, u32>,
    // 当一个房间被删除时会将房间ID存入，以便取用
    unuse_id: Vec<u32>,
}

impl AllRoomInfo {
    fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            unuse_id: Vec::new(),
        }
    }

    fn remove(&mut self, id: u32) -> RoomFull {
        let r = self.by_id.remove(&id).unwrap();
        self.by_name.remove(&r.name);
        self.unuse_id.push(id);
        r
    }
}

#[derive(Debug, Default)]
struct AllUserInfo {
    by_id: HashMap<ID, User>,
    by_name: HashMap<String, ID>,
    // 当一个房间被删除时会将房间ID存入，以便取用
    unuse_id: Vec<ID>,
}

impl AllUserInfo {
    fn insert(&mut self, u: &mut User) {
        if self.unuse_id.len() != 0 {
            u.id = self.unuse_id.pop().unwrap();
        } else {
            u.id = self.by_id.len() as ID;
            while let Some(_) = self.by_id.get(&u.id) {
                u.id += 1;
            }
        }
        self.by_name.insert(u.name.clone(), u.id);
        self.by_id.insert(u.id, u.clone());
    }

    fn remove(&mut self, id: ID) {
        let u = self.by_id.remove(&id).unwrap();
        self.by_name.remove(&u.name);
        self.unuse_id.push(id);
    }
}

#[derive(Debug)]
struct Process {
    user: User,
    stm: TcpStream,
    addr: SocketAddr,
    all_rooms: Arc<Mutex<AllRoomInfo>>,
    room: Vec<ID>,
    tx: mpsc::Sender<ClientInfo>,
    rx: mpsc::Receiver<ClientInfo>,
}

impl Process {
    fn new(user: User, stm: TcpStream, addr: SocketAddr, rooms: Arc<Mutex<AllRoomInfo>>) -> Self {
        let (tx, rx) = mpsc::channel::<ClientInfo>(64);
        Process {
            user, stm, addr, all_rooms: rooms,
            room: Vec::new(),
            tx, rx,
        }
    }

    async fn poll(&mut self) -> Result<()> {
        let mut reader = TryRead::new();
        loop {
            tokio::select! {
                res = self.stm.readable() => {
                    if let Err(_) = res {
                        break;
                    }
                    match reader.poll(&mut self.stm) {
                        Ok(_) => {
                            self.parse_pakage(&reader.package()).await?;
                        }
                        Err(e) => {
                            if let Some(e) = e.can_continue() {
                                warn!("{:?}", e);
                                break;
                            } else {
                                continue;
                            }
                        },
                    }
                },
                c = self.rx.recv() => {
                    if let Some(c) = c {
                        write(&mut self.stm, &serde_json::to_vec(&c)?).await?;
                    }
                },
                // 每五分钟确认一次客户端是否存在
                _ = sleep(Duration::from_secs(5 * 60)) => {
                    if let Err(_) = write(&mut self.stm, "".as_bytes()).await {
                        break;
                    };
                },
            }
        }
        Ok(())
    }

    async fn inst_room(&mut self, mut room: Room) -> Result<Room> {
        let mut lock = self.all_rooms.lock().await;
        let AllRoomInfo {
            by_id: rooms,
            by_name: rooms_by_name,
            unuse_id} = &mut lock as &mut AllRoomInfo;
        'a: loop {
            'new_room: loop {
                'join: loop {
                    if room.id != 0 {
                        if let Some(_) = rooms.get(&room.id) {
                            break 'join;
                        } else {
                            break 'new_room;
                        }
                    } else {
                        if let Some(rid) = rooms_by_name.get(&room.name) {
                            room.id = *rid;
                            break 'join;
                        } else {
                            if unuse_id.len() != 0 {
                                room.id = unuse_id.pop().unwrap();
                            } else {
                                room.id = rooms.len() as ID;
                                while let Some(_) = rooms.get(&room.id) {
                                    room.id += 1;
                                }
                            }
                            break 'new_room;
                        }
                    }
                    // break 'a;
                }
                // 加入房间
                let r = rooms.get_mut(&room.id).unwrap();
                if r.name == room.name && r.passwd == room.passwd {
                    // 发送加入成功，并将完整房间信息发送过去
                    write(&mut self.stm, "OK".as_bytes()).await?;
                    let rom = Room {
                        id: r.id,
                        name: r.name.clone(),
                        passwd: r.passwd.clone(),
                    };
                    write(&mut self.stm, &serde_json::to_vec(&rom)?).await?;

                    let mut cis = Vec::new();
                    let mut txs = Vec::new();
                    for (_, client) in r.cs.iter() {
                        let ci = ClientInfo{id: client.id, name: client.name.clone(), addr: client.addr.clone()};
                        txs.push(client.tx.clone());
                        cis.push(ci);
                    }
                    r.cs.insert(self.user.id, Client {
                        id: self.user.id,
                        name: self.user.name.clone(),
                        addr: self.addr.clone(),
                        tx: self.tx.clone(),
                    });
                    // 放开锁
                    drop(lock);
                    // 将所有房间内的客户端发送
                    write(&mut self.stm, &serde_json::to_vec(&cis).unwrap()).await?;
                    // 通知房间内的其他客户端连接
                    let cr_info = ClientInfo {
                        id: self.user.id,
                        name: self.user.name.clone(),
                        addr: self.addr.clone()
                    };
                    for tx in txs.iter() {
                        tx.send(cr_info.clone()).await.ok();
                    }
                } else {
                    return Err(std::io::ErrorKind::Other.into());
                }
                break 'a;
            }
            // 新建房间
            let mut cs = HashMap::new();
            cs.insert(self.user.id, Client {
                id: self.user.id,
                name: self.user.name.clone(),
                addr: self.addr.clone(),
                tx: self.tx.clone(),
            });
            let r = RoomFull { id: room.id, name: room.name.clone(), passwd: room.passwd.clone(), cs: cs };
            rooms.insert(room.id, r.clone());
            rooms_by_name.insert(room.name.clone(), room.id);
            info!("New: {:?}", room);
            write(&mut self.stm, "OK".as_bytes()).await?;
            let rom = Room {
                id: r.id,
                name: r.name.clone(),
                passwd: r.passwd.clone(),
            };
            write(&mut self.stm, &serde_json::to_vec(&rom)?).await?;
            write(&mut self.stm, &serde_json::to_vec(&Vec::<ClientInfo>::new()).unwrap()).await?;
            break;
        }
        // 记录房间
        self.room.push(room.id);
        Ok(room)
    }

    // 处理数据包
    async fn parse_pakage(&mut self, pkg: &[u8]) -> Result<()> {
        if let Ok(room) = serde_json::from_slice::<net::Room>(&pkg) {
            // 接收客户端传过来的房间信息
            let room = match self.inst_room(room).await {
                Ok(rom) => { rom },
                Err(_) => {
                    write(&mut self.stm, "Fail to join room".as_bytes()).await?;
                    return Ok(());
                }
            };
            info!("\"{}\" join \"{}\"", self.user.name, room.name);
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Client {
    id: ID,
    name: String,
    addr: SocketAddr,
    tx: mpsc::Sender<ClientInfo>,
}

impl Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("id", &self.id)
            .field("name", &self.name).field("addr", &self.addr)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct RoomFull {
    pub id: ID,
    name: String,
    passwd: String,
    cs: HashMap<ID, Client>,
}
