//! The chrome's program as SPIR-V, compiled from the shared WGSL by `build.rs`, and what its
//! vertex stage reads: a small reader of the module's vertex inputs, so a test can hold them
//! against `sighurt_render::gpu::VERTEX_ATTRIBUTES`.
//!
//! Must not own: the WGSL (sighurt-render's), or pipelines (`crate::device`).

#[cfg(test)]
use std::collections::HashMap;

#[cfg(test)]
use sighurt_render::gpu::AttributeFormat;

/// The program's SPIR-V words: both entry points, `vs_main` and `fs_main`.
pub fn words() -> Vec<u32> {
    let bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/chrome.spv"));
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

#[cfg(test)]
/// The vertex inputs of entry point `entry` (an `OpEntryPoint Vertex`): each `Location` and its
/// format, for the formats the chrome uses; `None` for a malformed module or another format.
pub fn vertex_inputs(words: &[u32], entry: &str) -> Option<Vec<(u32, AttributeFormat)>> {
    const ENTRY_POINT: u16 = 15;
    const DECORATE: u16 = 71;
    const VARIABLE: u16 = 59;
    const TYPE_POINTER: u16 = 32;
    const TYPE_VECTOR: u16 = 23;
    const TYPE_FLOAT: u16 = 22;
    const TYPE_INT: u16 = 21;
    const LOCATION: u32 = 30;
    const INPUT: u32 = 1;
    let mut interface = Vec::new();
    let mut locations = HashMap::new();
    let mut variables = HashMap::new();
    let mut pointers = HashMap::new();
    #[derive(Clone, Copy)]
    enum Type {
        Float(u32),
        Int(u32, bool),
        Vector(u32, u32),
    }
    let mut types = HashMap::new();
    let mut at = 5;
    while at < words.len() {
        let (count, op) = ((words[at] >> 16) as usize, words[at] as u16);
        let args = words.get(at + 1..at + count)?;
        match op {
            ENTRY_POINT if args.first() == Some(&0) => {
                let name: Vec<u8> = args[2..]
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .take_while(|&b| b != 0)
                    .collect();
                if name == entry.as_bytes() {
                    let words_of_name = name.len() / 4 + 1;
                    interface = args[2 + words_of_name..].to_vec();
                }
            }
            DECORATE if args.get(1) == Some(&LOCATION) => {
                locations.insert(args[0], *args.get(2)?);
            }
            VARIABLE if args.get(2) == Some(&INPUT) => {
                variables.insert(args[1], args[0]);
            }
            TYPE_POINTER => {
                pointers.insert(args[0], *args.get(2)?);
            }
            TYPE_FLOAT => {
                types.insert(args[0], Type::Float(*args.get(1)?));
            }
            TYPE_INT => {
                types.insert(args[0], Type::Int(*args.get(1)?, *args.get(2)? != 0));
            }
            TYPE_VECTOR => {
                types.insert(args[0], Type::Vector(*args.get(1)?, *args.get(2)?));
            }
            _ => {}
        }
        if count == 0 {
            return None;
        }
        at += count;
    }
    let mut inputs = Vec::new();
    for id in interface {
        let (Some(&location), Some(&pointer)) = (locations.get(&id), variables.get(&id)) else {
            continue;
        };
        let format = match types.get(pointers.get(&pointer)?)? {
            Type::Vector(component, 2) if matches!(types.get(component), Some(Type::Float(32))) => {
                AttributeFormat::Float32x2
            }
            Type::Vector(component, 4) if matches!(types.get(component), Some(Type::Float(32))) => {
                AttributeFormat::Float32x4
            }
            Type::Int(32, false) => AttributeFormat::Uint32,
            _ => return None,
        };
        inputs.push((location, format));
    }
    inputs.sort_by_key(|&(location, _)| location);
    Some(inputs)
}

#[cfg(test)]
mod tests {
    use sighurt_render::gpu::{VERTEX_ATTRIBUTES, VERTEX_ENTRY};

    use super::*;

    #[test]
    fn the_spirv_is_a_module_with_both_entry_points() {
        let words = words();
        assert_eq!(words[0], 0x0723_0203, "SPIR-V's magic number");
        assert!(vertex_inputs(&words, "vs_main").is_some_and(|inputs| !inputs.is_empty()));
        // Not a vertex entry point: no vertex inputs.
        assert_eq!(vertex_inputs(&words, "fs_main"), Some(Vec::new()));
    }

    /// The SPIR-V's vertex inputs are the Rust vertex layout's attributes.
    #[test]
    fn the_vertex_inputs_are_the_layouts() {
        let inputs = vertex_inputs(&words(), VERTEX_ENTRY).expect("readable");
        let expected: Vec<_> = VERTEX_ATTRIBUTES
            .iter()
            .map(|a| (a.location, a.format))
            .collect();
        assert_eq!(inputs, expected);
    }
}
