// Standalone NDJSON generator. Writes ~target_bytes worth of rows.
// Row shape: {"id":N,"name":"user_N","attributes":[{"key":"kI","value":"v_N_I"}, ...]}
//
// Build: rustc -O /tmp/gen_ndjson.rs -o /tmp/gen_ndjson
// Run:   /tmp/gen_ndjson <out_path> <target_bytes>

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};

const ATTRS_PER_ROW: usize = 5;

fn main() {
    let mut args = env::args().skip(1);
    let path = args.next().expect("usage: gen_ndjson <path> <bytes>");
    let target: u64 = args
        .next()
        .expect("usage: gen_ndjson <path> <bytes>")
        .parse()
        .expect("bytes must be integer");

    let file = File::create(&path).expect("create");
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let mut written: u64 = 0;
    let mut id: u64 = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(512);

    while written < target {
        id += 1;
        buf.clear();
        buf.extend_from_slice(b"{\"id\":");
        write_u64(&mut buf, id);
        buf.extend_from_slice(b",\"name\":\"user_");
        write_u64(&mut buf, id);
        buf.extend_from_slice(b"\",\"attributes\":[");
        for i in 1..=ATTRS_PER_ROW {
            if i > 1 {
                buf.push(b',');
            }
            buf.extend_from_slice(b"{\"key\":\"k");
            write_u64(&mut buf, i as u64);
            buf.extend_from_slice(b"\",\"value\":\"v_");
            write_u64(&mut buf, id);
            buf.push(b'_');
            write_u64(&mut buf, i as u64);
            buf.extend_from_slice(b"\"}");
        }
        buf.extend_from_slice(b"]}\n");
        w.write_all(&buf).expect("write");
        written += buf.len() as u64;
    }
    w.flush().expect("flush");
    eprintln!("wrote {} bytes, {} rows", written, id);
}

fn write_u64(buf: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        buf.push(b'0');
        return;
    }
    let start = buf.len();
    while n > 0 {
        buf.push(b'0' + (n % 10) as u8);
        n /= 10;
    }
    buf[start..].reverse();
}
