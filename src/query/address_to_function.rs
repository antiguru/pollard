//! Single-address symbol lookup for diagnostic and backfill use.
//!
//! When a profile shows hex offsets — typically because wholesym could not
//! load a system framework during the on-load symbolication pass — users
//! have historically had to run `nm` themselves to map an offset back to a
//! symbol. This tool wraps the same `wholesym::SymbolMap` lookup pollard
//! already does in `profile/symbolicate.rs`, but against a caller-supplied
//! relative address. See issue #9.

use crate::error::ToolError;
use crate::profile::Profile;
use crate::profile::raw::RawLib;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::Path;
use wholesym::{LookupAddress, SymbolManager, SymbolManagerConfig};

#[derive(Debug, Clone)]
pub struct Args {
    /// Library-relative address to resolve.
    pub address: u64,
    /// Optional substring matched against `lib.name`, `lib.debug_name`,
    /// `lib.path`, or `lib.debug_path`. When omitted, every loaded library
    /// is tried in order until one resolves the address.
    pub module: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    /// Outer (enclosing) function at the resolved address. When DWARF
    /// inlines exist, the deeper inlined callees are exposed via
    /// `inline_chain` (innermost-first), matching the convention in
    /// `profile/symbolicate.rs`.
    pub function: String,
    /// Library that resolved the address (`lib.name`, falling back to
    /// `lib.debug_name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Echo of the input address (relative).
    pub address: u64,
    /// Inlined call chain at this address, innermost-first. Empty when no
    /// DWARF inline records cover the address.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inline_chain: Vec<InlineEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InlineEntry {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

pub async fn address_to_function(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    // wholesym's `LookupAddress::Relative` is u32: a relative offset within
    // a single library's image always fits.
    let addr_u32: u32 = u32::try_from(args.address).map_err(|_| ToolError::Internal {
        message: format!(
            "address 0x{:x} does not fit in u32 (relative addresses are u32)",
            args.address
        ),
    })?;

    let candidates: Vec<&RawLib> = profile
        .all_libs()
        .filter(|lib| match args.module.as_deref() {
            None => true,
            Some(m) => lib_matches(lib, m),
        })
        .collect();

    if candidates.is_empty() {
        return Err(ToolError::Internal {
            message: match &args.module {
                Some(m) => format!("no library matches module filter {m:?}"),
                None => "profile contains no libraries".to_owned(),
            },
        });
    }

    let config = SymbolManagerConfig::new().use_spotlight(true);
    let symbol_manager = SymbolManager::with_config(config);

    let mut last_err: Option<String> = None;
    for lib in candidates {
        // Same path-resolution policy as `symbolicate::load_symbol_map_for_lib`:
        // prefer `path`, fall back to `debug_path`. Skip libs with neither.
        let path_str = match lib.path.as_deref().or(lib.debug_path.as_deref()) {
            Some(p) => p,
            None => continue,
        };
        let path = Path::new(path_str);
        let disambiguator = lib
            .arch
            .as_ref()
            .map(|arch| wholesym::MultiArchDisambiguator::Arch(arch.clone()));

        let map = match symbol_manager
            .load_symbol_map_for_binary_at_path(path, disambiguator)
            .await
        {
            Ok(map) => map,
            Err(e) => {
                last_err = Some(format!("{path_str}: {e}"));
                continue;
            }
        };

        let Some(addr_info) = map.lookup(LookupAddress::Relative(addr_u32)).await else {
            continue;
        };

        // `addr_info.frames` is innermost-first (samply-symbols convention).
        // The enclosing native function is the last entry; everything before
        // it is the inlined call chain.
        let outer = addr_info.frames.as_ref().and_then(|fs| fs.last());
        let function = outer
            .and_then(|f| f.function.as_deref())
            .unwrap_or(&addr_info.symbol.name)
            .to_owned();
        let file = outer
            .and_then(|f| f.file_path.as_ref())
            .map(|p| p.display_path().to_owned());
        let line = outer.and_then(|f| f.line_number);

        let inline_chain = match addr_info.frames.as_ref() {
            Some(frames) if frames.len() > 1 => frames[..frames.len() - 1]
                .iter()
                .map(|f| InlineEntry {
                    function: f.function.clone().unwrap_or_default(),
                    file: f.file_path.as_ref().map(|p| p.display_path().to_owned()),
                    line: f.line_number,
                })
                .collect(),
            _ => Vec::new(),
        };

        return Ok(Output {
            function,
            module: lib.name.clone().or_else(|| lib.debug_name.clone()),
            file,
            line,
            address: args.address,
            inline_chain,
        });
    }

    Err(ToolError::Internal {
        message: match last_err {
            Some(e) => format!(
                "address 0x{:x} not resolved (last lib error: {e})",
                args.address
            ),
            None => format!(
                "address 0x{:x} not resolved in any candidate library",
                args.address
            ),
        },
    })
}

fn lib_matches(lib: &RawLib, module: &str) -> bool {
    [
        lib.name.as_deref(),
        lib.debug_name.as_deref(),
        lib.path.as_deref(),
        lib.debug_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|s| s.contains(module))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib_with(name: Option<&str>, debug_name: Option<&str>, path: Option<&str>) -> RawLib {
        RawLib {
            name: name.map(str::to_owned),
            debug_name: debug_name.map(str::to_owned),
            path: path.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn lib_matches_substring_in_name() {
        let lib = lib_with(Some("libsystem_kernel.dylib"), None, None);
        assert!(lib_matches(&lib, "kernel"));
        assert!(!lib_matches(&lib, "libc"));
    }

    #[test]
    fn lib_matches_falls_back_to_debug_name() {
        let lib = lib_with(None, Some("MyApp"), None);
        assert!(lib_matches(&lib, "MyApp"));
    }

    #[test]
    fn lib_matches_path_substring() {
        let lib = lib_with(None, None, Some("/usr/lib/libc.dylib"));
        assert!(lib_matches(&lib, "libc"));
    }

    #[test]
    fn lib_matches_is_case_sensitive() {
        let lib = lib_with(Some("Foo"), None, None);
        assert!(!lib_matches(&lib, "foo"));
    }
}
