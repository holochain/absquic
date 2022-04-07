# absquic

Absquic quic transport library

#### Quickstart

```rust
use absquic::*;
use absquic::quinn::*;
use absquic::quinn_udp::*;

// get a in-process cached local self-signed ephemeral tls certificate pair
let (cert, pk) = dev_utils::localhost_self_signed_tls_cert();

let (udp_backend, max_gso_provider) = QuinnUdpBackendFactory::new(
    ([127, 0, 0, 1], 0).into(), // bind only to localhost
    None,                       // use default max_udp_size
);

let quic_backend = QuinnQuicBackendFactory::new(
    // from quinn udp backend
    max_gso_provider,

    // defaults
    QuinnEndpointConfig::default(),

    // simple server using the ephemeral self-signed cert
    Some(dev_utils::simple_server_config(cert, pk)),

    // trusting client that accepts any and all tls certs
    dev_utils::trusting_client_config(),
);

// bind the backends to get our endpoint handle and event receiver
let (mut endpoint, _evt_recv) = TokioRuntime::build_endpoint(
    udp_backend,
    quic_backend,
).await.unwrap();

// we can now make calls on the endpoint handle
assert!(endpoint.local_address().await.is_ok());

// and we could receive events on the event receiver like
// while let Some(evt) = _evt_recv.recv().await {}
```
