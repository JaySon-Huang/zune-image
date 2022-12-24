use std::fs::read;
use std::io::{Cursor, Read};
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

fn decode_writer_flate(bytes: &[u8]) -> Vec<u8>
{
    let mut writer = Vec::new();

    let mut deflater = flate2::read::ZlibDecoder::new(Cursor::new(bytes));

    deflater.read_to_end(&mut writer).unwrap();

    writer
}

fn decode_writer_zune(bytes: &[u8]) -> Vec<u8>
{
    let mut deflater = zune_inflate::DeflateDecoder::new(bytes);

    deflater.decode_zlib().unwrap()
}

fn decode_test(c: &mut Criterion)
{
    let path = env!("CARGO_MANIFEST_DIR").to_string() + "/tests/zlib/tt.zlib";

    let data = read(path).unwrap();

    let mut group = c.benchmark_group("ZLIB decoding");
    group.throughput(Throughput::Bytes(data.len() as u64));

    // group.bench_function("FLATE-[ZLIB-NG]", |b| {
    //     b.iter(|| black_box(decode_writer_flate(data.as_slice())))
    // });

    group.bench_function("ZUNE", |b| {
        b.iter(|| black_box(decode_writer_zune(data.as_slice())))
    });
}
criterion_group!(name=benches;
      config={
      let c = Criterion::default();
        c.measurement_time(Duration::from_secs(20))
      };
    targets=decode_test);

criterion_main!(benches);