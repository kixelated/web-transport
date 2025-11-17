import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

const RESET_STREAM = 0x04;
const STOP_SENDING = 0x05;
const STREAM = 0x08;
const STREAM_FIN = 0x09;
const APPLICATION_CLOSE = 0x1d;

export interface Data {
	type: "stream";
	id: Stream.Id;
	data: Uint8Array;
	fin: boolean;
	// no offset, because everything is ordered
	// no length, because WebSocket already provides this
}

export interface ResetStream {
	type: "reset_stream";
	id: Stream.Id;
	code: VarInt;
	// no final size, because there's no flow control
}

export interface StopSending {
	type: "stop_sending";
	id: Stream.Id;
	code: VarInt;
}

export interface ConnectionClose {
	type: "connection_close";
	code: VarInt;
	// no reason size, because WebSocket already provides this.
	reason: string;
}

export interface Padding {
	type: "padding";
}

export interface Ping {
	type: "ping";
}

export type Any = Data | ResetStream | StopSending | ConnectionClose;

export function encode(frame: Any): Uint8Array {
	switch (frame.type) {
		case "stream": {
			// Calculate the maximum size of the buffer we'll need
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + frame.data.length), 0, 1);

			buffer[0] = frame.fin ? STREAM_FIN : STREAM;
			buffer = frame.id.value.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
			buffer.set(frame.data, buffer.byteLength - frame.data.length);

			return buffer;
		}

		case "reset_stream": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);

			buffer[0] = RESET_STREAM;
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		case "stop_sending": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);

			buffer[0] = STOP_SENDING;
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		case "connection_close": {
			const body = new TextEncoder().encode(frame.reason);
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + body.length), 0, 1);

			buffer[0] = APPLICATION_CLOSE;
			buffer = frame.code.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
			buffer.set(body, buffer.byteLength - body.length);

			return buffer;
		}
	}
}

export function decode(buffer: Uint8Array): Any {
	if (buffer.length === 0) {
		throw new Error("Invalid frame: empty buffer");
	}

	const frameType = buffer[0];
	buffer = buffer.slice(1);

	let v: VarInt;

	if (frameType === RESET_STREAM) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return {
			type: "reset_stream",
			id,
			code,
		};
	}

	if (frameType === STOP_SENDING) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return {
			type: "stop_sending",
			id,
			code,
		};
	}

	if (frameType === APPLICATION_CLOSE) {
		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		const reason = new TextDecoder().decode(buffer);

		return {
			type: "connection_close",
			code,
			reason,
		};
	}

	if (frameType === STREAM || frameType === STREAM_FIN) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		return {
			type: "stream",
			id,
			data: buffer,
			fin: frameType === STREAM_FIN,
		};
	}

	throw new Error(`Invalid frame type: ${frameType}`);
}
