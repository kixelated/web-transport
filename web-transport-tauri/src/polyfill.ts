import { Buffer } from "buffer";
import * as rpc from "./rpc.ts";

export default class WebTransportTauri implements WebTransport {
	readonly closed: Promise<WebTransportCloseInfo>;
	readonly datagrams = new WebTransportDatagramDuplexStreamTauri();
	readonly incomingBidirectionalStreams: ReadableStream;
	readonly incomingUnidirectionalStreams: ReadableStream;
	readonly ready: Promise<void>;

	#url: URL;
	#session: Promise<number>;

	constructor(url: string | URL, options?: WebTransportOptions) {
		this.#url = url instanceof URL ? url : new URL(url);

		// Ignore options.allowPooling and options.requireUnreliable for now.

		const hashes = options?.serverCertificateHashes?.map((hash) => {
			if (hash.algorithm !== "sha-256") {
				throw new Error("Only sha-256 is currently supported");
			}

			if (hash.value === undefined) {
				throw new Error("Server certificate hash is required");
			}

			return Buffer.from(hash.value as ArrayBuffer).toString("hex");
		});

		// Connect via Tauri backend
		this.#session = rpc
			.connect({
				url: this.#url.toString(),
				congestionControl: options?.congestionControl,
				serverCertificateHashes: hashes,
			})
			.then((response) => response.session);

		this.ready = this.#session.then(() => {});

		this.closed = this.#session.then((session) =>
			rpc.closed({
				session,
			}),
		);

		this.incomingBidirectionalStreams =
			new ReadableStream<WebTransportBidirectionalStream>({
				pull: async (controller) => {
					const accept = await rpc.accept({
						session: await this.#session,
						bidirectional: true,
					});

					const readable = this.#createReadable(accept.stream);
					const writable = this.#createWritable(accept.stream);

					controller.enqueue({ readable, writable });
				},
			});

		this.incomingUnidirectionalStreams = new ReadableStream<
			ReadableStream<Uint8Array>
		>({
			pull: async (controller) => {
				const accept = await rpc.accept({
					session: await this.#session,
					bidirectional: false,
				});

				const readable = this.#createReadable(accept.stream);
				controller.enqueue(readable);
			},
		});
	}

	#createReadable(stream: number): ReadableStream<Uint8Array> {
		return new ReadableStream<Uint8Array>({
			pull: async (controller) => {
				const response = await rpc.read({
					session: await this.#session,
					stream,
				});

				if (response.data) {
					controller.enqueue(response.data);
				} else {
					controller.close();
				}
			},
			cancel: async () => {
				await rpc.reset({
					session: await this.#session,
					stream,
					code: 0,
				});
			},
		});
	}

	#createWritable(stream: number): WritableStream<Uint8Array> {
		return new WritableStream<Uint8Array>({
			write: async (chunk) => {
				await rpc.write({
					session: await this.#session,
					stream,
					data: chunk,
				});
			},
			close: async () => {
				await rpc.write({
					session: await this.#session,
					stream,
					data: null,
				});
			},
		});
	}

	close(closeInfo?: WebTransportCloseInfo): void {
		this.#session.then((session) =>
			rpc.close({
				session,
				code: closeInfo?.closeCode ?? 0,
				reason: closeInfo?.reason ?? null,
			}),
		);
	}

	async createBidirectionalStream(
		options?: WebTransportSendStreamOptions,
	): Promise<WebTransportBidirectionalStream> {
		const response = await rpc.open({
			session: await this.#session,
			bidirectional: true,
			sendOrder: options?.sendOrder,
		});

		return {
			readable: this.#createReadable(response.stream),
			writable: this.#createWritable(response.stream),
		};
	}

	async createUnidirectionalStream(
		options?: WebTransportSendStreamOptions,
	): Promise<WritableStream> {
		const response = await rpc.open({
			session: await this.#session,
			bidirectional: false,
			sendOrder: options?.sendOrder,
		});

		return this.#createWritable(response.stream);
	}
}

// TODO Implement this
export class WebTransportDatagramDuplexStreamTauri
	implements WebTransportDatagramDuplexStream
{
	incomingHighWaterMark: number;
	incomingMaxAge: number | null;
	readonly maxDatagramSize: number;
	outgoingHighWaterMark: number;
	outgoingMaxAge: number | null;
	readonly readable: ReadableStream;
	readonly writable: WritableStream;

	constructor() {
		this.incomingHighWaterMark = 1024;
		this.incomingMaxAge = null;
		this.maxDatagramSize = 1200;
		this.outgoingHighWaterMark = 1024;
		this.outgoingMaxAge = null;
		this.readable = new ReadableStream<Uint8Array>({});
		this.writable = new WritableStream<Uint8Array>({});
	}
}
