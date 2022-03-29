# absquic

Absquic quic transport library

#### Quickstart

```rust
use absquic::*;
use absquic_quinn::dev_utils;

// get a in-process cached local self-signed ephemeral tls certificate pair
let (cert, pk) = dev_utils::localhost_self_signed_tls_cert();

// construct a new endpoint factory using the quinn backend types
let endpoint_factory = EndpointFactory::new(
    // use the quinn-udp udp backend
    QuinnUdpBackendFactory::new(
        ([127, 0, 0, 1], 0).into(), // bind only to localhost
        None,                       // use default max_udp_size
    ),

    // use the quinn-proto powered backend driver
    QuinnDriverFactory::new(
        // defaults
        QuinnEndpointConfig::default(),

        // simple server using the ephemeral self-signed cert
        Some(dev_utils::simple_server_config(cert, pk)),

        // trusting client that accepts any and all tls certs
        dev_utils::trusting_client_config(),
    ),
);

// bind the actual udp port
let (mut endpoint, _evt_recv, driver) = endpoint_factory
    .bind(TokioTimeoutsScheduler::new())
    .await
    .unwrap();

// spawn the backend driver future
tokio::task::spawn(driver);

// we can now make calls on the endpoint handle
assert!(endpoint.local_address().await.is_ok());

// and we could receive events on the event receiver like
// while let Some(evt) = _evt_recv.recv().await {}
```
