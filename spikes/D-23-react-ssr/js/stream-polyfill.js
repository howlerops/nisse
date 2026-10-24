// ReadableStream for engines that lack it, from web-streams-polyfill.
import { ReadableStream } from "web-streams-polyfill";
if (typeof globalThis.ReadableStream === "undefined") globalThis.ReadableStream = ReadableStream;
