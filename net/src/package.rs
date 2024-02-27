use std::{borrow::BorrowMut, result};

use tokio::{
    net::TcpStream,
    io::*,
};

#[derive(Debug)]
pub enum ErrorType {
    IO(std::io::Error),
    // 当前接收到的数据不是一个数据包
    NotPakage(Vec<u8>),
    // 未能完整接收数据包头
    MissingHead(Vec<u8>),
    // 传输中断
    TransmissionInterrupted(Vec<u8>),
    Other(Vec<u8>),
    None,
}

impl ErrorType {
    pub fn can_continue(self) -> Option<ErrorType> {
        match self {
            Self::None | Self::NotPakage(_) => {
                None
            },
            Self::MissingHead(vec) => {
                if vec.len() == 0 {
                    Some(Self::MissingHead(vec))
                } else {
                    None
                }
            },
            Self::IO(e) => {
                if let std::io::ErrorKind::WouldBlock = e.kind() {
                    None
                } else {
                    Some(Self::IO(e))
                }
            },
            e => { Some(e) }
        }
    }
}

fn verify_head(buf: &[u8; 8]) -> Option<u32> {
    let mut u32_byte_buf: [u8; 4] = [0u8; 4];
    u32_byte_buf.copy_from_slice(&buf[..4]);
    let slen = u32::from_be_bytes(u32_byte_buf);
    u32_byte_buf.copy_from_slice(&buf[4..]);
    let rslen = u32::from_be_bytes(u32_byte_buf);
    if slen != !rslen {
        None
    } else {
        Some(slen)
    }
}

pub async fn read(stm: &mut TcpStream) -> result::Result<Vec<u8>, ErrorType> {
    let mut len_buf: [u8; 8] = [0; 8];
    match stm.read(&mut len_buf).await {
        Ok(siz) => {
            if siz != 8 {
                let mut v: Vec<u8> = Vec::with_capacity(siz);
                v.extend_from_slice(&len_buf[..siz]);
                return Err(ErrorType::MissingHead(v));
            }
        },
        Err(e) => { return Err(ErrorType::IO(e)); },
    }
    let len = if let Some(len) = verify_head(&len_buf) { len as usize }
                        else { return Err(ErrorType::NotPakage(Vec::from(len_buf))); };
    let mut data: Vec<u8> = Vec::with_capacity(len);
    unsafe { data.set_len(len); }
    let rlen = match stm.read(&mut data).await {
        Ok(len) => { len },
        Err(e) => {
            return Err(ErrorType::IO(e));
        },
    };
    if rlen != len {
        return Err(ErrorType::TransmissionInterrupted(data));
    }
    Ok(data)
}

#[derive(Debug)]
pub struct TryRead {
    pkg: Vec<u8>,
    len_buf: TryReadBuf,
}

#[derive(Debug)]
enum TryReadBuf {
    BUF(([u8; 8], usize)),
    LEN(u32),
}

impl TryReadBuf {
    fn len(&self) -> Option<u32> {
        match self {
            Self::BUF(_) => None,
            Self::LEN(l) => Some(*l),
        }
    }
}

impl TryRead {
    pub fn new() -> Self {
        Self {
            pkg: Vec::with_capacity(0),
            len_buf: TryReadBuf::BUF(([0; 8], 0)),
        }
    }

    pub fn poll(&mut self, stm: &mut TcpStream) -> result::Result<u32, ErrorType> {
        if let TryReadBuf::BUF((len_buf, buf_len)) = self.len_buf.borrow_mut() {
            let rlen = match stm.try_read(&mut len_buf[*buf_len..]) {
                Ok(len) => { len },
                Err(e) => { return Err(ErrorType::IO(e)); },
            };
            if rlen == 0 {
                return Err(ErrorType::MissingHead(Vec::new()));
            }
            *buf_len += rlen;
            if *buf_len < 8 {
                return Err(ErrorType::None);
            }
            let pkg_len = if let Some(len) = verify_head(&len_buf) { len }
                            else {
                                self.pkg = Vec::with_capacity(0);
                                *buf_len = 0;
                                return Err(ErrorType::NotPakage(Vec::from(len_buf)));
                            };
            self.pkg = Vec::with_capacity(pkg_len as usize);
            self.len_buf = TryReadBuf::LEN(pkg_len);
        }
        let pkg_len = if let TryReadBuf::LEN(l) = self.len_buf { l } else { 0 };
        if self.len_buf.len().unwrap() as usize == self.pkg.len() {
            return Ok(0);
        }
        let old_len = self.pkg.len();
        unsafe { self.pkg.set_len(pkg_len as usize) }
        let rlen = {
            match stm.try_read(&mut self.pkg[old_len..]) {
                Ok(l) => l,
                Err(e) => {
                    unsafe { self.pkg.set_len(old_len) };
                    return Err(ErrorType::IO(e));
                }
            }
        };
        unsafe { self.pkg.set_len(rlen + old_len) }
        if self.pkg.len() == pkg_len as usize {
            return Ok(rlen as u32);
        }
        return Err(ErrorType::None);
    }

    pub fn status(&self)  {

    }

    pub fn clear(&mut self) {
        self.pkg = Vec::with_capacity(0);
        self.len_buf = TryReadBuf::BUF(([0; 8], 0));
    }

    pub fn package(&mut self) -> Vec<u8> {
        self.len_buf = TryReadBuf::BUF(([0; 8], 0));
        std::mem::take(&mut self.pkg)
    }
}

pub async fn write(stm: &mut TcpStream, data: &[u8]) -> Result<()> {
    let slen = data.len() as u32;
    let mut len_buf = [0u8; 8];
    len_buf[..4].copy_from_slice(&slen.to_be_bytes());
    len_buf[4..].copy_from_slice(&(!slen).to_be_bytes());
    stm.write_all(&len_buf).await?;
    stm.write_all(&data).await?;
    Ok(())
}
