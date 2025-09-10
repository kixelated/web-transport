import { invoke } from "@tauri-apps/api/core";

export interface ConnectRequest {
	url: string;

	congestionControl?: "default" | "low-latency" | "throughput";
	// Only hex-encoded sha256 is currently supported.
	serverCertificateHashes?: string[];
}

export interface ConnectResponse {
	session: number;
}

export async function connect(payload: ConnectRequest): Promise<ConnectResponse> {
	return invoke<ConnectResponse>("plugin:web-transport|connect", {
		payload,
	});
}

export async function close(payload: CloseRequest): Promise<CloseResponse> {
	return invoke<CloseResponse>("plugin:web-transport|close", { payload });
}

export interface CloseRequest {
	session: number;
	code: number;
	reason: string | null;
}

export type CloseResponse = Record<string, never>;

export async function closed(payload: ClosedRequest): Promise<ClosedResponse> {
	return invoke<ClosedResponse>("plugin:web-transport|closed", { payload });
}

export interface ClosedRequest {
	session: number;
}

export type ClosedResponse = Record<string, never>;

export async function accept(payload: AcceptRequest): Promise<AcceptResponse> {
	return invoke<AcceptResponse>("plugin:web-transport|accept", { payload });
}

export interface AcceptRequest {
	session: number;
	bidirectional: boolean;
}

export interface AcceptResponse {
	stream: number;
}

export async function open(payload: OpenRequest): Promise<OpenResponse> {
	return invoke<OpenResponse>("plugin:web-transport|open", { payload });
}

export interface OpenRequest {
	session: number;
	bidirectional: boolean;
	sendOrder?: number;
}

export interface OpenResponse {
	stream: number;
}

export async function read(payload: ReadRequest): Promise<ReadResponse> {
	return invoke<ReadResponse>("plugin:web-transport|read", { payload });
}

export interface ReadRequest {
	session: number;
	stream: number;
}

export interface ReadResponse {
	data: Uint8Array | null;
}

export async function write(payload: WriteRequest): Promise<WriteResponse> {
	return invoke<WriteResponse>("plugin:web-transport|write", {
		payload,
	});
}

export interface WriteRequest {
	session: number;
	stream: number;
	data: Uint8Array | null;
}

export type WriteResponse = Record<string, never>;

export async function reset(payload: ResetRequest): Promise<ResetResponse> {
	return invoke<ResetResponse>("plugin:web-transport|reset", {
		payload,
	});
}

export interface ResetRequest {
	session: number;
	stream: number;
	code: number;
}

export type ResetResponse = Record<string, never>;
