use std::io;
use std::mem::{offset_of, size_of};
use std::ptr;

use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;

pub fn owned(process: u32, port: u16) -> io::Result<bool> {
    let mut bytes = 0_u32;
    // SAFETY: the null sizing query only writes the live size output.
    let code = unsafe {
        GetExtendedTcpTable(
            ptr::null_mut(),
            &mut bytes,
            0,
            u32::from(AF_INET),
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if code != ERROR_INSUFFICIENT_BUFFER && code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    for _ in 0..3 {
        if !(4..=16 * 1024 * 1024).contains(&bytes) {
            return Err(invalid("TCP listener table exceeds its bound"));
        }
        // u32 storage supplies the alignment required by the IPv4 table layout.
        let mut storage = vec![0_u32; (bytes as usize).div_ceil(size_of::<u32>())];
        let capacity = storage.len() * size_of::<u32>();
        bytes = capacity as u32;
        // SAFETY: storage owns at least bytes writable memory, aligned for DWORDs.
        let code = unsafe {
            GetExtendedTcpTable(
                storage.as_mut_ptr().cast(),
                &mut bytes,
                0,
                u32::from(AF_INET),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if code == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        if bytes as usize > capacity || bytes < 4 {
            return Err(invalid("TCP listener table returned an invalid length"));
        }
        let count = storage[0] as usize;
        let start = offset_of!(MIB_TCPTABLE_OWNER_PID, table);
        let needed = count
            .checked_mul(size_of::<MIB_TCPROW_OWNER_PID>())
            .and_then(|length| start.checked_add(length))
            .ok_or_else(|| invalid("TCP listener table size overflow"))?;
        if needed > bytes as usize {
            return Err(invalid("TCP listener table is truncated"));
        }
        for index in 0..count {
            // SAFETY: the checked row range lies inside initialized storage.
            let row = unsafe {
                ptr::read_unaligned(
                    storage
                        .as_ptr()
                        .cast::<u8>()
                        .add(start + index * size_of::<MIB_TCPROW_OWNER_PID>())
                        .cast::<MIB_TCPROW_OWNER_PID>(),
                )
            };
            if row.dwOwningPid == process
                && row.dwLocalAddr.to_ne_bytes() == [127, 0, 0, 1]
                && u16::from_be(row.dwLocalPort as u16) == port
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "TCP listener table changed during bounded queries",
    ))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
