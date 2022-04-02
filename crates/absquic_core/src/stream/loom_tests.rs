use super::*;
use crate::sync::block_on;
use crate::sync::thread;
use crate::tokio_runtime::TokioRuntime;
use bytes::Buf;

fn write_stream_front_drop_inner() {
    const DATA: &[u8] = b"hello world!";

    let (mut back, mut front) = write_stream_pair::<TokioRuntime>(3);

    let hnd = thread::spawn(move || {
        block_on(async move {
            match front
                .write_bytes_all(&mut bytes::Bytes::from_static(DATA))
                .await
            {
                Ok(_) => (),
                Err(e) => panic!("unexpected err: {:?}", e),
            }
        });
    });

    let mut rdata = bytes::BytesMut::new();

    while rdata.remaining() < DATA.len() {
        if block_on(async {
            match back.recv().await {
                WriteCmd::Data(mut data) => {
                    let len = data.len();
                    rdata.extend_from_slice(&data.split_to(len));
                    false
                }
                WriteCmd::Stop(_) => true,
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
fn write_stream_front_drop() {
    write_stream_front_drop_inner();
}

#[test]
#[cfg(loom)]
fn loom_write_stream_front_drop() {
    loom::model(|| {
        write_stream_front_drop_inner();
    });
}

fn write_stream_back_drop_inner() {
    const DATA: &[u8] = b"abc";

    let (mut back, mut front) = write_stream_pair::<TokioRuntime>(3);

    let hnd = thread::spawn(move || {
        block_on(async move {
            loop {
                match front
                    .write_bytes_all(&mut bytes::Bytes::from_static(DATA))
                    .await
                {
                    Ok(_) => (),
                    Err(_) => break,
                }
            }
        });
    });

    for _ in 0..3 {
        block_on(async {
            match back.recv().await {
                WriteCmd::Data(mut data) => {
                    let len = data.len();
                    let data = data.split_to(len);
                    assert_eq!(DATA, data.as_ref());
                }
                WriteCmd::Stop(_) => {
                    panic!("unexpected stop");
                }
            }
        });
    }

    drop(back);

    hnd.join().unwrap();
}

#[test]
#[cfg(not(loom))]
fn write_stream_back_drop() {
    write_stream_back_drop_inner();
}

#[test]
#[cfg(loom)]
fn loom_write_stream_back_drop() {
    loom::model(|| {
        write_stream_back_drop_inner();
    });
}

fn read_stream_front_drop_inner() {
    const DATA: &[u8] = b"abc";

    let (mut back, mut front) = read_stream_pair::<TokioRuntime>(3);

    let hnd = thread::spawn(move || {
        block_on(async move {
            loop {
                match back.acquire().await {
                    Ok(sender) => sender.send(bytes::Bytes::from_static(DATA)),
                    Err(_) => break,
                }
            }
        });
    });

    for _ in 0..3 {
        block_on(async {
            match front.read_bytes(usize::MAX).await {
                Some(Ok(data)) => {
                    assert_eq!(DATA, data.as_ref());
                }
                oth => panic!("unexpected result: {:?}", oth),
            }
        })
    }

    drop(front);

    hnd.join().unwrap();
}

#[test]
#[cfg(not(loom))]
fn read_stream_front_drop() {
    read_stream_front_drop_inner();
}

#[test]
#[cfg(loom)]
fn loom_read_stream_front_drop() {
    loom::model(|| {
        read_stream_front_drop_inner();
    });
}

fn read_stream_back_drop_inner() {
    const DATA: &[u8] = b"hello world!";

    let (mut back, mut front) = read_stream_pair::<TokioRuntime>(3);

    let hnd = thread::spawn(move || {
        block_on(async move {
            let mut to_send = bytes::Bytes::from_static(DATA);
            while !to_send.is_empty() {
                let permit = match back.acquire().await {
                    Ok(permit) => permit,
                    Err(_) => panic!("unexpected send failure"),
                };
                let max_len = permit.max_len();
                permit.send(to_send.split_to(max_len));
            }
        });
    });

    let mut rdata = bytes::BytesMut::new();

    while rdata.remaining() < DATA.len() {
        if block_on(async {
            match front.read_bytes(usize::MAX).await {
                Some(Ok(data)) => {
                    rdata.extend_from_slice(data.as_ref());
                    false
                }
                Some(Err(e)) => panic!("unexpected err: {:?}", e),
                None => true,
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
fn read_stream_back_drop() {
    read_stream_back_drop_inner();
}

#[test]
#[cfg(loom)]
fn loom_read_stream_back_drop() {
    loom::model(|| {
        read_stream_back_drop_inner();
    });
}
