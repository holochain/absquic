/*
use crate::*;
use absquic_core::backend::util::*;
use absquic_core::util::*;

#[tokio::test(flavor = "multi_thread")]
async fn test_udp() {
    let mut driver_tasks = Vec::new();
    let mut test_tasks = Vec::new();

    let socket1 = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);
    let socket1 = socket1.bind().await.unwrap();
    let addr1 = socket1.local_addr().unwrap();
    println!("socket1 addr: {:?}", addr1);

    let socket2 = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);
    let socket2 = socket2.bind().await.unwrap();
    let addr2 = socket2.local_addr().unwrap();
    println!("socket2 addr: {:?}", addr2);

    let (send1, recv1, driver1, shutdown1) = udp_test(socket1);
    let (send2, recv2, driver2, shutdown2) = udp_test(socket2);

    // cross-link these senders / receivers
    // so ep1 waits on ep2 and the reverse
    let (ep2_s, ep1_r) = one_shot_channel::<()>();
    let (ep1_s, ep2_r) = one_shot_channel::<()>();

    // -- endpoint 1 -- //

    driver_tasks.push(tokio::task::spawn(driver1));
    test_tasks.push(tokio::task::spawn(async move {
        let mut recv1 = recv1;
        for _ in 0..2 {
            println!("recv1: {:?}", recv1.recv().await);
        }
        ep1_s.send(&mut WakeLater::new(), ());
        println!("recv1 ending");
    }));
    test_tasks.push(tokio::task::spawn(async move {
        let addr2 = &addr2;
        for _ in 0..2 {
            send1
                .send(OutUdpPacket {
                    dst_addr: *addr2,
                    ecn: None,
                    data: b"hello".to_vec(),
                    segment_size: None,
                    src_ip: None,
                })
                .unwrap();
        }
        let _ = ep1_r.recv().await;
    }));

    // -- endpoint 2 -- //

    driver_tasks.push(tokio::task::spawn(driver2));
    test_tasks.push(tokio::task::spawn(async move {
        let mut recv2 = recv2;
        for _ in 0..2 {
            println!("recv2: {:?}", recv2.recv().await);
        }
        ep2_s.send(&mut WakeLater::new(), ());
        println!("recv2 ending");
    }));
    test_tasks.push(tokio::task::spawn(async move {
        let addr1 = &addr1;
        for _ in 0..2 {
            send2
                .send(OutUdpPacket {
                    dst_addr: *addr1,
                    ecn: None,
                    data: b"world".to_vec(),
                    segment_size: None,
                    src_ip: None,
                })
                .unwrap();
        }
        let _ = ep2_r.recv().await;
    }));

    // make sure the tests all pass
    futures::future::try_join_all(test_tasks).await.unwrap();

    // shutdown the endpoints
    shutdown1();
    shutdown2();

    // make sure the endpoint drivers shut down
    futures::future::try_join_all(driver_tasks).await.unwrap();
}
*/
