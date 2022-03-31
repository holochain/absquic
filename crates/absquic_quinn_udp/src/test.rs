/*
use crate::*;

#[tokio::test(flavor = "multi_thread")]
async fn test_udp() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(
            tracing_subscriber::filter::EnvFilter::from_default_env(),
        )
        .with_file(true)
        .with_line_number(true)
        .without_time()
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let factory = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);

    let (mut s1, r1, d1) = factory.bind().await.unwrap();
    let t1 = tokio::task::spawn(d1);

    let (mut s2, mut r2, d2) = factory.bind().await.unwrap();
    let t2 = tokio::task::spawn(d2);

    let addr2 = s2.local_addr().await.unwrap();
    println!("addr2: {:?}", addr2);

    let (os, or) = tokio::sync::oneshot::channel();

    let tr2 = tokio::task::spawn(async move {
        let data = r2.recv().await;
        let _ = os.send(());

        println!("r2 got: {:#?}", data);
        assert_eq!(b"hello", data.unwrap().data.as_ref());

        // make sure the channel ends
        while let Some(_) = r2.recv().await {}
    });

    println!("sending");

    // udp is unreliable, but we should get at least 1 out of 5 : )
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;

        s1.send(OutUdpPacket {
            dst_addr: addr2,
            ecn: None,
            data: b"hello".to_vec(),
            segment_size: None,
            src_ip: None,
        })
        .await
        .unwrap();
    }

    println!("awaiting recv");
    or.await.unwrap();

    println!("dropping handles");
    drop(s1);
    drop(r1);
    drop(s2);

    println!("await recv task");
    tr2.await.unwrap();
    println!("await d1");
    t1.await.unwrap();
    println!("await d2");
    t2.await.unwrap();
}
*/
