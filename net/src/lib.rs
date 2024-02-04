pub mod package;
pub mod room;

pub use package::*;
pub use room::*;
use std::net::SocketAddr;

#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Debug, Clone)]
pub struct BaseUserInfo {
    pub id: u32,
    pub name: String,
}

/// 每一个客户端对应的信息
#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: u32,
    pub name: String,
    pub addr: SocketAddr,
}

pub trait ToPackage {
    fn package(&self) -> Result<Vec<u8>, serde_json::Error>;
}

#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Debug, Default, Clone)]
pub struct User {
    pub id: u32,
    pub name: String,
    pub passwd: String,
}

impl User {
    pub fn new() -> Self {
        Default::default()
    }
}

impl ToPackage for User {
    fn package(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}
