//! Inspect import tables from explicitly supplied PE bytes without executing them.

use object::read::pe::{ImageNtHeaders, PeFile, PeFile32, PeFile64};
use object::{FileKind, LittleEndian as LE};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Import {
    pub(crate) library: String,
    pub(crate) delayed: bool,
    pub(crate) python_runtime: bool,
}

pub(crate) fn inspect(data: &[u8]) -> Result<Vec<Import>, String> {
    match FileKind::parse(data).map_err(|e| format!("invalid PE: {e}"))? {
        FileKind::Pe32 => inspect_file(PeFile32::parse(data).map_err(|e| e.to_string())?),
        FileKind::Pe64 => inspect_file(PeFile64::parse(data).map_err(|e| e.to_string())?),
        _ => Err("expected a PE executable or DLL".into()),
    }
}

fn inspect_file<'a, Pe: ImageNtHeaders>(file: PeFile<'a, Pe>) -> Result<Vec<Import>, String> {
    let mut imports = Vec::new();
    let directories = file.data_directories();
    for index in [1, 13] {
        if let Some(entry) = directories.iter().nth(index) {
            let address = entry.virtual_address.get(LE);
            let size = entry.size.get(LE);
            if (address == 0) != (size == 0) {
                return Err("inconsistent PE import directory".into());
            }
        }
    }
    if let Some(table) = file.import_table().map_err(|e| e.to_string())? {
        let mut descriptors = table.descriptors().map_err(|e| e.to_string())?;
        while let Some(descriptor) = descriptors.next().map_err(|e| e.to_string())? {
            let name = table
                .name(descriptor.name.get(LE))
                .map_err(|e| e.to_string())?;
            add_import(&mut imports, name, false)?;
        }
    }
    let sections = file.section_table();
    if let Some(table) = directories
        .delay_load_import_table(file.data(), &sections)
        .map_err(|e| e.to_string())?
    {
        let mut descriptors = table.descriptors().map_err(|e| e.to_string())?;
        while let Some(descriptor) = descriptors.next().map_err(|e| e.to_string())? {
            // Only RVA-based delay descriptors are supported; old VA-based or
            // reserved attribute combinations must never silently pass the gate.
            if descriptor.attributes.get(LE) != 1 {
                return Err("unsupported PE delay import attributes".into());
            }
            let name = table
                .name(descriptor.dll_name_rva.get(LE))
                .map_err(|e| e.to_string())?;
            add_import(&mut imports, name, true)?;
        }
    }
    Ok(imports)
}

fn add_import(imports: &mut Vec<Import>, name: &[u8], delayed: bool) -> Result<(), String> {
    if name.is_empty() || !name.is_ascii() {
        return Err("invalid PE import library name".into());
    }
    let library = std::str::from_utf8(name)
        .map_err(|e| e.to_string())?
        .to_owned();
    imports.push(Import {
        python_runtime: python_dll(&library),
        library,
        delayed,
    });
    Ok(())
}

pub(crate) fn python_dll(name: &str) -> bool {
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    let Some(stem) = name.strip_suffix(".dll") else {
        return false;
    };
    let stem = stem.strip_suffix("_d").unwrap_or(stem);
    let Some(version) = stem
        .strip_prefix("python")
        .or_else(|| stem.strip_prefix("libpython"))
    else {
        return false;
    };
    version.is_empty() || version.chars().all(|c| c.is_ascii_digit() || c == '.')
}

#[cfg(test)]
#[path = "pe_imports_tests.rs"]
pub(crate) mod tests;
