use std::{borrow::BorrowMut, io};

use tokio::{
    net::TcpStream,
    io::*,
};

pub async fn read(stm: &mut TcpStream) -> Result<Vec<u8>> {
    let mut len_buf: [u8; 4] = [0; 4];
    stm.read(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    let mut data: Vec<u8> = Vec::with_capacity(len as usize);
    unsafe { data.set_len(len as usize); }
    let rlen = stm.read(&mut data).await?;
    if rlen != len as usize {
        panic!("fail to read, {}:{}", len, rlen);
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
    BUF(([u8; 4], usize)),
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
            len_buf: TryReadBuf::BUF(([0; 4], 0)),
        }
    }

    pub fn poll(&mut self, stm: &mut TcpStream) -> Result<()> {
        if let TryReadBuf::BUF((len_buf, buf_len)) = self.len_buf.borrow_mut() {
            let rlen = stm.try_read(&mut len_buf[*buf_len..])?;
            if rlen == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            *buf_len += rlen;
            if *buf_len < 4 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let pkg_len = u32::from_be_bytes(len_buf[..4].try_into().unwrap());
            self.pkg = Vec::with_capacity(pkg_len as usize);
            self.len_buf = TryReadBuf::LEN(pkg_len);
        }
        let pkg_len = if let TryReadBuf::LEN(l) = self.len_buf { l } else { 0 };
        if self.len_buf.len().unwrap() as usize == self.pkg.len() {
            return Ok(());
        }
        let old_len = self.pkg.len();
        unsafe { self.pkg.set_len(pkg_len as usize) }
        let rlen = {
            match stm.try_read(&mut self.pkg[old_len..]) {
                Ok(l) => l,
                Err(e) => {
                    unsafe { self.pkg.set_len(old_len) };
                    return Err(e);
                }
            }
        };
        unsafe { self.pkg.set_len(rlen + old_len) }
        if self.pkg.len() == pkg_len as usize {
            return Ok(());
        }
        return Err(io::ErrorKind::WouldBlock.into());
    }

    pub fn clear(&mut self) {
        self.pkg = Vec::with_capacity(0);
        self.len_buf = TryReadBuf::BUF(([0; 4], 0));
    }

    pub fn package(&mut self) -> Vec<u8> {
        self.len_buf = TryReadBuf::BUF(([0; 4], 0));
        std::mem::take(&mut self.pkg)
    }
}

pub async fn write(stm: &mut TcpStream, data: &[u8]) -> Result<()> {
    stm.write_all(&(data.len() as u32).to_be_bytes()).await?;
    stm.write_all(&data).await?;
    Ok(())
}
