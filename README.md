# WebTransport

WebTransport is a protocol for client-server communication over QUIC.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which is then layered on top of QUIC.
This library hides that detail and tries to expose only the QUIC API, delegating as much as possible to the underlying QUIC implementation.

QUIC provides two primary APIs:

## Streams

QUIC streams are ordered, reliable, flow-controlled, and optionally bidirectional.
Both endpoints can create and close streams (including an error code) with no overhead.
You can think of them as TCP connections, but shared over a single QUIC connection.

## Datagrams

QUIC datagrams are unordered, unreliable, and not flow-controlled.
Both endpoints can send datagrams below the MTU size (~1.2kb minimum) and they might arrive out of order or not at all.
They are basically UDP packets, except they are encrypted and congestion controlled.

# Crates

This project is broken up into a few different crates:

-   [web-transport-quinn](web-transport-quinn) mirrors the [Quinn API](https://docs.rs/quinn/latest/quinn/index.html), abstracting away the HTTP/3 setup.
-   [web-transport-wasm](web-transport-wasm) wraps the [browser API](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API)
-   [web-transport-proto](web-transport-proto) a bare minimum implementation of HTTP/3 just to establish the WebTransport session.
-   [web-transport](web-transport) provides a generic interface, delegating to [web-transport-quinn](web-transport-quinn) or [web-transport-wasm](web-transport-wasm) depending on the platform.
