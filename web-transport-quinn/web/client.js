// @ts-ignore embed the certificate fingerprint using bundler
import fingerprintHex from 'bundle-text:../cert/localhost.hex';

// Convert the hex to binary.
const fingerprint = Uint8Arra.fromHex(fingerprint);

const params = new URLSearchParams(window.location.search)

const url = params.get("url") || "https://localhost:4443"
const datagram = params.get("datagram") || false

function log(msg) {
    const element = document.createElement("div");
    element.innerText = msg;

    document.body.appendChild(element);
}

async function run() {
    // Connect using the hex fingerprint in the cert folder.
    const transport = new WebTransport(url, {
        serverCertificateHashes: [{
            "algorithm": "sha-256",
            "value": fingerprint
        }],
    });
    await transport.ready;

    log("connected");

    let writer;
    let reader;

    if (!datagram) {
        // Create a bidirectional stream
        const stream = await transport.createBidirectionalStream();
        log("created stream");

        writer = stream.writable.getWriter();
        reader = stream.readable.getReader();
    } else {
        log("using datagram");

        // Create a datagram
        writer = transport.datagrams.writable.getWriter();
        reader = transport.datagrams.readable.getReader();
    }

    // Create a message
    const msg = 'Hello, world!';
    const encoded = new TextEncoder().encode(msg);

    await writer.write(encoded);
    await writer.close();
    writer.releaseLock();

    log("send: " + msg);

    // Read a message from it
    // TODO handle partial reads
    const { value } = await reader.read();

    const recv = new TextDecoder().decode(value);
    log("recv: " + recv);

    transport.close();
    log("closed");
}

run();
