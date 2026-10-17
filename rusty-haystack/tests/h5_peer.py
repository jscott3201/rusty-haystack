"""Handwritten Jeto peer; no production codecs generate expected wire values."""
import json

from .async_peer import Peer


def boxed(spec, val):
    return {"spec": "sys::" + spec, "val": val}


RICH = {
    "id": boxed("Ref", "rich"),
    "integer": boxed("Int", "9223372036854775807"),
    "float": boxed("Float", "-0.0"),
    "number": boxed("Number", "-0.0kW"),
    "none": boxed("None", "∅"),
    "bytes": boxed("Buf", "AP8"),
    "list": [None, boxed("Marker", "✓")],
}


class H5Peer(Peer):
    async def operation(self, reader, writer, method, path, headers, body):
        if not path.startswith("/api/sys.api::readById"):
            return False
        assert method == "POST"
        assert headers["xeto-version"] == "5"
        assert headers["content-type"] == "application/json;box=all"
        assert headers["accept"] == "application/json;box=all"
        if self.mode == "stall_h5":
            writer.write(b"HTTP/1.1 200 Fixture\r\nContent-Type: application/json;box=all\r\nXeto-Version: 5\r\nContent-Length: 100\r\n\r\n{")
            await writer.drain()
            await reader.read()
            self.eof += 1
            await self.notify()
            return True
        if self.mode in ("oversize_declared", "oversize_chunked"):
            head = b"Content-Length: 1048577\r\n" if self.mode == "oversize_declared" else b"Transfer-Encoding: chunked\r\n"
            writer.write(b"HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nXeto-Version: 5\r\n" + head + b"\r\n")
            if self.mode == "oversize_chunked":
                data = b"x" * 1048577
                writer.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            await writer.drain()
            await reader.read()
            self.eof += 1
            await self.notify()
            return True
        request = json.loads(body)
        checked = request["checked"] == boxed("Bool", "true")
        def entity(ref):
            assert ref["spec"] == "sys::Ref"
            return RICH if ref["val"] == "rich" else None
        if path.endswith("readByIds"):
            rows = [entity(ref) for ref in request["ids"]]
            missing = any(row is None for row in rows)
            payload = {"spec": "sys::Grid", "cols": [{"name": name} for name in RICH], "rows": [row or {name: None for name in RICH} for row in rows]}
        else:
            payload = entity(request["id"]) if "id" in request else None
            missing = payload is None
        status, content_type, version = 200, "application/json;box=all", "5"
        if missing and checked:
            status = 404
            payload = {"spec": "sys.api::UnknownEntityErr", "status": 404, "dis": "private-peer-sentinel"}
        if self.mode == "batch_extra":
            payload["rows"].append(payload["rows"][0])
        if self.mode == "batch_swapped":
            payload["rows"].reverse()
        if self.mode == "checked_placeholder":
            payload["rows"] = [{} for _ in request["ids"]]
        if self.mode == "permission":
            status, payload = 403, {"spec": "sys.api::PermissionErr", "status": 418, "dis": "private-peer-sentinel"}
        if self.mode == "unknown_error":
            status, payload = 400, {"spec": "evil::PrivateErr", "dis": "private-peer-sentinel"}
        if self.mode == "invalid_error":
            status, payload = 400, {"spec": "sys.api::PermissionErr", "status": "private-peer-sentinel"}
        if self.mode == "unknown_value":
            payload = {"id": boxed("Ref", "rich"), "secret": {"spec": "evil::Private", "val": "private-peer-sentinel"}}
        if self.mode == "wrong_shape":
            payload = []
        if self.mode == "wrong_identity":
            payload = {"id": boxed("Ref", "other")}
        if self.mode == "submicrosecond":
            payload = {"id": boxed("Ref", "rich"), "time": boxed("Time", "12:00:00.000000001")}
        if self.mode == "wrong_media":
            content_type = "application/json;v=3"
        if self.mode == "wrong_version":
            version = "4"
        if self.mode == "duplicate_box":
            content_type = "application/json;box=all;box=auto"
        if self.mode == "partial_status":
            status = 206
        extra = "Content-Encoding: gzip\r\n" if self.mode == "content_encoding" else ""
        encoded = json.dumps(payload).encode() if self.mode != "invalid_json" else b"private-peer-sentinel"
        writer.write(f"HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nXeto-Version: {version}\r\n{extra}Content-Length: {len(encoded)}\r\n\r\n".encode() + encoded)
        await writer.drain()
        return True
