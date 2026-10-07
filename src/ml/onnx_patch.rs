//! A small edit to an ONNX file in memory: making a graph's input and
//! output dimensions symbolic.
//!
//! YuNet's published file declares its input as `1 × 3 × 640 × 640`
//! although the network itself is fully convolutional and its reshapes
//! are written as `[1, -1, k]`: OpenCV's DNN ignores the declared shape
//! and runs it at any size, but ONNX Runtime refuses a tensor of another
//! shape.  Measured here (scratch, Python), the same graph with symbolic
//! dims runs at 832 × 832 and in batches of four, its outputs flattened
//! in batch order.  So the helper rewrites the declared dims after the
//! file's digest has been checked, never on disk: the pinned file stays
//! the publisher's.
//!
//! This is a protobuf walk over the few messages involved (`ModelProto`
//! → `GraphProto` → `ValueInfoProto` → `TypeProto` → `Tensor` →
//! `TensorShapeProto` → `Dimension`), re-encoding each enclosing message
//! because the lengths change.  Everything else is copied byte for byte.

/// A protobuf field as read: its number, wire type and raw payload (for a
/// varint, the value; for a length-delimited field, the bytes).
#[derive(Debug, Clone)]
enum Field<'a> {
    Varint(u32, u64),
    Fixed64(u32, [u8; 8]),
    Bytes(u32, &'a [u8]),
    Fixed32(u32, [u8; 4]),
}

fn read_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut out = 0u64;
    let mut shift = 0;
    loop {
        let &byte = b.get(*i)?;
        *i += 1;
        out |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(out);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Split a message into its fields (none if it isn't well formed).
fn fields(b: &[u8]) -> Option<Vec<Field<'_>>> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let tag = read_varint(b, &mut i)?;
        let (num, wire) = ((tag >> 3) as u32, tag & 7);
        out.push(match wire {
            0 => Field::Varint(num, read_varint(b, &mut i)?),
            1 => {
                let v: [u8; 8] = b.get(i..i + 8)?.try_into().ok()?;
                i += 8;
                Field::Fixed64(num, v)
            }
            2 => {
                let n = read_varint(b, &mut i)? as usize;
                let v = b.get(i..i + n)?;
                i += n;
                Field::Bytes(num, v)
            }
            5 => {
                let v: [u8; 4] = b.get(i..i + 4)?.try_into().ok()?;
                i += 4;
                Field::Fixed32(num, v)
            }
            _ => return None,
        });
    }
    Some(out)
}

fn write_field(out: &mut Vec<u8>, f: &Field<'_>) {
    match f {
        Field::Varint(n, v) => {
            write_varint(out, ((*n as u64) << 3) | 0);
            write_varint(out, *v);
        }
        Field::Fixed64(n, v) => {
            write_varint(out, ((*n as u64) << 3) | 1);
            out.extend_from_slice(v);
        }
        Field::Bytes(n, v) => {
            write_bytes_field(out, *n, v);
        }
        Field::Fixed32(n, v) => {
            write_varint(out, ((*n as u64) << 3) | 5);
            out.extend_from_slice(v);
        }
    }
}

fn write_bytes_field(out: &mut Vec<u8>, num: u32, v: &[u8]) {
    write_varint(out, ((num as u64) << 3) | 2);
    write_varint(out, v.len() as u64);
    out.extend_from_slice(v);
}

/// Re-encode a message, replacing the payload of every length-delimited
/// field numbered `num` with `f(payload)`.
fn map_field(msg: &[u8], num: u32, f: &dyn Fn(&[u8]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(msg.len() + 16);
    for field in fields(msg)? {
        match field {
            Field::Bytes(n, v) if n == num => write_bytes_field(&mut out, n, &f(v)?),
            other => write_field(&mut out, &other),
        }
    }
    Some(out)
}

/// A `Dimension` with `dim_param` = `name` (and no `dim_value`).
fn symbolic_dim(dim: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for field in fields(dim)? {
        match field {
            // dim_value (1) and dim_param (2) are replaced; denotation (3)
            // and anything else stay.
            Field::Varint(1, _) | Field::Bytes(2, _) => {}
            other => write_field(&mut out, &other),
        }
    }
    write_bytes_field(&mut out, 2, name.as_bytes());
    Some(out)
}

/// A `TensorShapeProto` with the dims at `which` (all when empty) made
/// symbolic, named `d<i>`.
fn symbolic_shape(shape: &[u8], which: &[usize]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    for field in fields(shape)? {
        match field {
            Field::Bytes(1, dim) => {
                if which.is_empty() || which.contains(&i) {
                    write_bytes_field(&mut out, 1, &symbolic_dim(dim, &format!("d{i}"))?);
                } else {
                    write_bytes_field(&mut out, 1, dim);
                }
                i += 1;
            }
            other => write_field(&mut out, &other),
        }
    }
    Some(out)
}

/// A `ValueInfoProto` (an input or output) with its tensor shape's dims
/// at `which` made symbolic: type (2) → tensor_type (1) → shape (2).
fn symbolic_value(value: &[u8], which: &[usize]) -> Option<Vec<u8>> {
    map_field(value, 2, &|ty| map_field(ty, 1, &|tensor| map_field(tensor, 2, &|shape| symbolic_shape(shape, which))))
}

/// The model with every graph input's dims at `input_dims` (all when
/// empty) and every output's dims made symbolic.  `None` if the file
/// isn't a readable ONNX model.
pub fn make_dims_symbolic(model: &[u8], input_dims: &[usize]) -> Option<Vec<u8>> {
    // ModelProto.graph is field 7; GraphProto.input 11, output 12.
    map_field(model, 7, &|graph| {
        let with_inputs = map_field(graph, 11, &|v| symbolic_value(v, input_dims))?;
        map_field(&with_inputs, 12, &|v| symbolic_value(v, &[]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny hand-built model: graph { input "x" [1, 3, 640, 640];
    /// output "y" [1, 6400, 1]; name "g" }, with an ir_version first.
    fn tiny_model() -> Vec<u8> {
        let dim_value = |v: u64| {
            let mut d = Vec::new();
            write_field(&mut d, &Field::Varint(1, v));
            d
        };
        let shape = |dims: &[u64]| {
            let mut s = Vec::new();
            for &d in dims {
                write_bytes_field(&mut s, 1, &dim_value(d));
            }
            s
        };
        let value = |name: &str, dims: &[u64]| {
            let mut tensor = Vec::new();
            write_field(&mut tensor, &Field::Varint(1, 1)); // elem_type FLOAT
            write_bytes_field(&mut tensor, 2, &shape(dims));
            let mut ty = Vec::new();
            write_bytes_field(&mut ty, 1, &tensor);
            let mut v = Vec::new();
            write_bytes_field(&mut v, 1, name.as_bytes());
            write_bytes_field(&mut v, 2, &ty);
            v
        };
        let mut graph = Vec::new();
        write_bytes_field(&mut graph, 11, &value("x", &[1, 3, 640, 640]));
        write_bytes_field(&mut graph, 12, &value("y", &[1, 6400, 1]));
        write_bytes_field(&mut graph, 2, b"g");
        let mut model = Vec::new();
        write_field(&mut model, &Field::Varint(1, 8)); // ir_version
        write_bytes_field(&mut model, 7, &graph);
        model
    }

    /// The dims of the first input and output as (value, param) pairs.
    fn dims_of(model: &[u8], field: u32) -> Vec<(Option<u64>, Option<String>)> {
        let graph = fields(model).unwrap().into_iter().find_map(|f| match f {
            Field::Bytes(7, g) => Some(g),
            _ => None,
        })
        .unwrap();
        let value = fields(graph).unwrap().into_iter().find_map(|f| match f {
            Field::Bytes(n, v) if n == field => Some(v),
            _ => None,
        })
        .unwrap();
        let ty = fields(value).unwrap().into_iter().find_map(|f| if let Field::Bytes(2, t) = f { Some(t) } else { None }).unwrap();
        let tensor = fields(ty).unwrap().into_iter().find_map(|f| if let Field::Bytes(1, t) = f { Some(t) } else { None }).unwrap();
        let shape = fields(tensor).unwrap().into_iter().find_map(|f| if let Field::Bytes(2, s) = f { Some(s) } else { None }).unwrap();
        fields(shape)
            .unwrap()
            .into_iter()
            .filter_map(|f| if let Field::Bytes(1, d) = f { Some(d) } else { None })
            .map(|d| {
                let fs = fields(d).unwrap();
                let v = fs.iter().find_map(|f| if let Field::Varint(1, v) = f { Some(*v) } else { None });
                let p = fs.iter().find_map(|f| if let Field::Bytes(2, p) = f { Some(String::from_utf8_lossy(p).into_owned()) } else { None });
                (v, p)
            })
            .collect()
    }

    #[test]
    fn varints_round_trip() {
        for v in [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut b = Vec::new();
            write_varint(&mut b, v);
            let mut i = 0;
            assert_eq!(read_varint(&b, &mut i), Some(v));
            assert_eq!(i, b.len());
        }
        assert_eq!(read_varint(&[0x80], &mut 0), None, "a truncated varint");
    }

    #[test]
    fn the_chosen_input_dims_and_every_output_dim_become_symbolic() {
        let model = tiny_model();
        assert_eq!(dims_of(&model, 11), [(Some(1), None), (Some(3), None), (Some(640), None), (Some(640), None)]);
        let patched = make_dims_symbolic(&model, &[0, 2, 3]).expect("patched");
        assert_eq!(
            dims_of(&patched, 11),
            [(None, Some("d0".into())), (Some(3), None), (None, Some("d2".into())), (None, Some("d3".into()))],
            "batch, height and width symbolic; channels kept"
        );
        assert_eq!(dims_of(&patched, 12), [(None, Some("d0".into())), (None, Some("d1".into())), (None, Some("d2".into()))]);
        // The rest of the file is untouched: the graph name and the
        // ir_version are still there, in order.
        let fs = fields(&patched).unwrap();
        assert!(matches!(fs[0], Field::Varint(1, 8)));
        let graph = fields(&patched).unwrap().into_iter().find_map(|f| if let Field::Bytes(7, g) = f { Some(g.to_vec()) } else { None }).unwrap();
        assert!(fields(&graph).unwrap().iter().any(|f| matches!(f, Field::Bytes(2, b"g"))));
        // Patching twice is the same as once; garbage is refused.
        assert_eq!(make_dims_symbolic(&patched, &[0, 2, 3]).unwrap(), patched);
        assert_eq!(make_dims_symbolic(b"\xff\xff\xff", &[]), None);
        assert_eq!(make_dims_symbolic(b"", &[]), Some(Vec::new()));
    }
}
