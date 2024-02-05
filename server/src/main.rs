use tokio::net::{TcpListener, TcpStream};
use std::collections::HashMap;
use std::env;
use std::fmt::Debug;
use std::process::exit;
use std::{net::SocketAddr, str, sync::Arc};
use net::*;
use tokio::sync::*;
use tokio::io::*;
use tokio::time::sleep;
use std::time::Duration;

const LISTEN_ADDR: &str = "0.0.0.0:5566";

#[tokio::main]
async fn main() {
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
                    println!("请检查该地址是否正确：{}", addr);
                    exit(1);
                }
            },
            rooms: Arc::new(Mutex::new(AllRoomInfo::new())),
            users: Arc::new(Mutex::new(AllUserInfo::default())),
        }
    }

    async fn run(self) {
        println!("server run in {}", self.addr);

        let t1 = tokio::spawn(Self::poll_cmd(self.rooms.clone(), self.users.clone()));
        let t2 = tokio::spawn(Self::accept(self.listener, self.rooms.clone(), self.users.clone()));
        tokio::join!(t1, t2).0.unwrap();
    }

    /// 处理新接入的客户端
    /// 创建一个任务处理
    async fn accept(listener: TcpListener,  rooms: Arc<Mutex<AllRoomInfo>>, users: Arc<Mutex<AllUserInfo>>) {
        loop {
            let (stm, addr) = listener.accept().await.unwrap();
            // 创建任务处理
            tokio::spawn(CertificationCenter::poll(stm, addr, rooms.clone(), users.clone()));
        }
    }

    /// 处理标准输入
    async fn poll_cmd(rooms: Arc<Mutex<AllRoomInfo>>, users: Arc<Mutex<AllUserInfo>>) {
        loop {
            let mut reader = BufReader::new(stdin());
            let mut buf = String::new();
            reader.read_line(&mut buf).await.unwrap();
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
        let mut user = Self::wait_login(&mut stm, users.clone()).await.unwrap();
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
        println!("{}: {:?}", &addr, &user);
        let uid = user.id;
        let mut prcs = Process::new(user, stm, addr, rooms.clone());
        prcs.poll().await.ok();
        {
            let mut users = users.lock().await;
            users.remove(uid);
            println!("{}: Quit {:?}", prcs.addr, prcs.user);
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
                    rom.cs.len()
                };
                // 如果房间为空了就删除房间
                if len == 0 {
                    let rom = lock.remove(*rid);
                    println!("{:?} was destroyed", rom);
                }
            }
        }
    }

    /// 等待用户登录
    /// 成功返回用户信息
    async fn wait_login(mut stm: &mut TcpStream, users: Arc<Mutex<AllUserInfo>>) -> Result<User> {
        loop {
            let pack = read(stm).await?;
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
    by_id: HashMap<u32, User>,
    by_name: HashMap<String, u32>,
    // 当一个房间被删除时会将房间ID存入，以便取用
    unuse_id: Vec<u32>,
}

impl AllUserInfo {
    fn insert(&mut self, u: &mut User) {
        if self.unuse_id.len() != 0 {
            u.id = self.unuse_id.pop().unwrap();
        } else {
            u.id = self.by_id.len() as u32;
            while let Some(_) = self.by_id.get(&u.id) {
                u.id += 1;
            }
        }
        self.by_name.insert(u.name.clone(), u.id);
        self.by_id.insert(u.id, u.clone());
    }

    fn remove(&mut self, id: u32) {
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
    room: Vec<u32>,
}

impl Process {
    fn new(user: User, stm: TcpStream, addr: SocketAddr, rooms: Arc<Mutex<AllRoomInfo>>) -> Self {
        Process {
            user, stm, addr, all_rooms: rooms,
            room: Vec::new()
        }
    }

    async fn poll(&mut self) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<ClientInfo>(64);
        // 接收客户端传过来的房间信息
        let _ = loop {
            match self.inst_room(tx.clone()).await {
                Ok(r) => break r,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    write(&mut self.stm, "Fail to join room".as_bytes()).await?;
                }
                Err(e) => { return Err(e); }
            }
        };
        let mut reader = TryRead::new();
        loop {
            tokio::select! {
                res = self.stm.readable() => {
                    if let Err(_) = res {
                        break;
                    }
                    match reader.poll(&mut self.stm) {
                        Ok(rlen) => {
                            if rlen == 0 { break; }
                            reader.clear();
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => { }
                        Err(_) => { break; }
                    }
                },
                c = rx.recv() => {
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

    async fn inst_room(&mut self, tx: mpsc::Sender<ClientInfo>) -> Result<Room> {
        let mut room: Room = {
            let pkg = read(&mut self.stm).await?;
            if let Ok(rom) = serde_json::from_slice(pkg.as_slice()) {
                rom
            } else {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
        };
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
                                room.id = rooms.len() as u32;
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
                        tx: tx.clone(),
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
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                break 'a;
            }
            // 新建房间
            let mut cs = HashMap::new();
            cs.insert(self.user.id, Client {
                id: self.user.id,
                name: self.user.name.clone(),
                addr: self.addr.clone(),
                tx: tx.clone(),
            });
            let r = RoomFull { id: room.id, name: room.name.clone(), passwd: room.passwd.clone(), cs: cs };
            rooms.insert(room.id, r.clone());
            rooms_by_name.insert(room.name.clone(), room.id);
            println!("New: {:?}", room);
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
}

#[derive(Clone)]
struct Client {
    id: u32,
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
    pub id: u32,
    name: String,
    passwd: String,
    cs: HashMap<u32, Client>,
}
