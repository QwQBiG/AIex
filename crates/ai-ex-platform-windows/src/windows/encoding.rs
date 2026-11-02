use std::cmp::Ordering;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Globalization::{CSTR_EQUAL, CSTR_LESS_THAN, CompareStringOrdinal};

use crate::ProcessLaunch;

pub fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut encoded: Vec<_> = value.encode_wide().collect();
    if encoded.contains(&0) {
        return Err(invalid("launch text contains NUL"));
    }
    encoded.push(0);
    Ok(encoded)
}

pub fn command(launch: &ProcessLaunch) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    for argument in std::iter::once(launch.executable.as_os_str())
        .chain(launch.arguments.iter().map(|item| item.as_os_str()))
    {
        let text = wide(argument)?;
        if !line.is_empty() {
            line.push(32);
        }
        line.push(34);
        let mut slashes = 0;
        for &unit in &text[..text.len() - 1] {
            if unit == 92 {
                slashes += 1;
                continue;
            }
            line.extend(std::iter::repeat_n(
                92,
                slashes * if unit == 34 { 2 } else { 1 },
            ));
            if unit == 34 {
                line.push(92);
            }
            line.push(unit);
            slashes = 0;
        }
        line.extend(std::iter::repeat_n(92, slashes * 2));
        line.push(34);
    }
    line.push(0);
    if line.len() > 32_767 {
        return Err(invalid("launch command exceeds Windows limit"));
    }
    Ok(line)
}

pub fn environment(launch: &ProcessLaunch) -> io::Result<Vec<u16>> {
    let mut entries = Vec::new();
    for (key, value) in &launch.environment {
        let key = wide(key)?;
        if key.len() == 1 || key.contains(&61) {
            return Err(invalid("invalid environment key"));
        }
        entries.push((key, wide(value)?));
    }
    // Windows requires case-insensitive Unicode ordering, independent of locale.
    let mut compare_error = None;
    entries.sort_by(|left, right| match compare(&left.0, &right.0) {
        Ok(ordering) => ordering,
        Err(error) => {
            compare_error.get_or_insert(error);
            Ordering::Equal
        }
    });
    if let Some(error) = compare_error {
        return Err(error);
    }
    for adjacent in entries.windows(2) {
        if compare(&adjacent[0].0, &adjacent[1].0)?.is_eq() {
            return Err(invalid(
                "environment contains duplicate case-insensitive keys",
            ));
        }
    }
    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend_from_slice(&key[..key.len() - 1]);
        block.push(61);
        block.extend_from_slice(&value);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    if block.len() > 1024 * 1024 {
        return Err(invalid("launch environment exceeds limit"));
    }
    Ok(block)
}

fn compare(left: &[u16], right: &[u16]) -> io::Result<Ordering> {
    // SAFETY: this module supplies terminated strings produced by wide().
    match unsafe { CompareStringOrdinal(left.as_ptr(), -1, right.as_ptr(), -1, 1) } {
        CSTR_EQUAL => Ok(Ordering::Equal),
        CSTR_LESS_THAN => Ok(Ordering::Less),
        0 => Err(io::Error::last_os_error()),
        _ => Ok(Ordering::Greater),
    }
}

pub fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
