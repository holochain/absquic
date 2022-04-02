use crate::*;
use absquic_core::runtime::AsyncRuntime;
use absquic_core::tokio_runtime::TokioRuntime;

async fn test_udp<Runtime: AsyncRuntime>() {
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

    let (cmd1, send1, recv1) = factory.bind::<TokioRuntime>().await.unwrap();
    let (s, r) = Runtime::one_shot();
    cmd1.acquire()
        .await
        .unwrap()
        .send(UdpBackendCmd::GetLocalAddress(s));
    let addr1 = r.await.unwrap().unwrap();
    tracing::warn!("addr1: {:?}", addr1);

    let (cmd2, send2, mut recv2) =
        factory.bind::<TokioRuntime>().await.unwrap();
    let (s, r) = Runtime::one_shot();
    cmd2.acquire()
        .await
        .unwrap()
        .send(UdpBackendCmd::GetLocalAddress(s));
    let addr2 = r.await.unwrap().unwrap();
    tracing::warn!("addr2: {:?}", addr2);

    let (os, or) = Runtime::one_shot();

    let task = Runtime::spawn(async move {
        let data = recv2.recv().await;
        os.send(());

        match data.unwrap() {
            UdpBackendEvt::InUdpPacket(packet) => {
                tracing::warn!("recv2 got: {:#?}", packet);
                assert_eq!(b"hello", packet.data.as_ref());
            }
        }

        // make sure the channel ends
        while let Some(_) = recv2.recv().await {}

        Ok(())
    });

    tracing::warn!("sending");

    // udp is unreliable, but we should get at least 1 out of 5 : )
    for _ in 0..5 {
        Runtime::sleep(
            std::time::Instant::now() + std::time::Duration::from_millis(1),
        )
        .await;

        send1.acquire().await.unwrap().send(OutUdpPacket {
            dst_addr: addr2,
            ecn: None,
            data: b"hello".to_vec(),
            segment_size: None,
            src_ip: None,
        });
    }

    tracing::warn!("awaiting recv");
    or.await.unwrap();

    tracing::warn!("dropping handles");
    cmd1.acquire()
        .await
        .unwrap()
        .send(UdpBackendCmd::CloseImmediate);
    drop(cmd1);
    drop(send1);
    drop(recv1);
    cmd2.acquire()
        .await
        .unwrap()
        .send(UdpBackendCmd::CloseImmediate);
    drop(cmd2);
    drop(send2);

    tracing::warn!("await recv task");
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_udp_tokio() {
    test_udp::<TokioRuntime>().await;
}
