use super::*;
use crate::sync::block_on;
use crate::sync::thread;
use bytes::Buf;

fn write_stream_smoke_inner() {
    const DATA: &[u8] = b"hello world!";

    let (mut back, mut front) = write_stream_pair(3);

    let hnd = thread::spawn(move || {
        //println!("starting write thread");
        block_on(async move {
            //println!("starting write op");
            match front
                .write_bytes_all(&mut bytes::Bytes::from_static(DATA))
                .await
            {
                Ok(_) => (),
                Err(e) => println!("{}", e),
            }
            //println!("write op done");
        });
        //println!("write thread done");
    });

    let mut rdata = bytes::BytesMut::new();

    while rdata.remaining() < DATA.len() {
        if block_on(async {
            match back.recv().await {
                WriteCmd::Data(mut data) => {
                    //println!("READ '{}'", String::from_utf8_lossy(&data));
                    let len = data.len();
                    rdata.extend_from_slice(&data.split_to(len));
                    false
                }
                WriteCmd::Stop(_) => {
                    //println!("READ Stop");
                    true
                }
            }
        }) {
            break;
        }
    }

    assert_eq!(DATA, rdata.copy_to_bytes(rdata.remaining()).as_ref());

    hnd.join().unwrap();
}

#[test]
#[cfg(not(loom))]
fn write_stream_smoke() {
    write_stream_smoke_inner();
}

#[test]
#[cfg(loom)]
fn loom_write_stream_smoke() {
    loom::model(|| {
        write_stream_smoke_inner();
    });
}
