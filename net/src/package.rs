use std::io;

use tokio::{
    net::TcpStream,
    io::*,
};

pub async fn read(stm: &mut TcpStream) -> Result<Vec<u8>> {
    let len = stm.read_u32().await?;
    let mut data: Vec<u8> = Vec::with_capacity(len as usize);
    data.resize(len as usize, 0);
    let rlen = stm.read(&mut data).await?;
    if rlen != len as usize {
        panic!("fail to read, {}:{}", len, rlen);
    }
    Ok(data)
}

#[derive(Debug, Default)]
pub struct TryRead {
    pkg: Vec<u8>,
    len: Option<u32>,
}

impl TryRead {
    pub fn new() -> Self {
        Self {
            pkg: Vec::with_capacity(0),
            ..Default::default()
        }
    }

    pub fn poll(&mut self, stm: &mut TcpStream) -> Result<()> {
        if let None = self.len {
            let n = self.pkg.capacity();
            let mut len_buf: Vec<u8> = Vec::with_capacity(4 - n);
            unsafe { len_buf.set_len(4 - n) }
            let rlen = stm.try_read(&mut len_buf)?;
            unsafe { self.pkg.set_len(n); }
            self.pkg.reserve(rlen + n);
            self.pkg.extend_from_slice(&len_buf[..rlen]);
            unsafe { self.pkg.set_len(0); }
            if rlen < 4 - n {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.pkg = Vec::with_capacity(rlen);
            self.len = Some(rlen as u32);
        }
        let old_len = self.pkg.len();
        unsafe { self.pkg.set_len(self.len.unwrap() as usize) }
        let rlen = stm.try_read(&mut self.pkg[old_len..])?;
        unsafe { self.pkg.set_len(rlen + old_len) }
        if self.pkg.len() == self.pkg.capacity() {
            return Ok(());
        }
        return Err(io::ErrorKind::WouldBlock.into());
    }

    pub fn clear(&mut self) {
        self.pkg = Vec::with_capacity(0);
        self.len = None;
    }
}

pub async fn write(stm: &mut TcpStream, data: &[u8]) -> Result<()> {
    stm.write_u32(data.len() as u32).await?;
    stm.write_all(&data).await?;
    Ok(())
}
