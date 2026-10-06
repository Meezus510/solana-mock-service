"""NDJSON social provider adapter, deliberately independent of Signal cursors."""

import json
import sys

from fixture_transport import Transport

PROTOCOL = "social-evidence-telegram-v1"


def main():
    transport = Transport()
    channels = transport.call("/__mock/telegram/channels")
    for line in sys.stdin:
        req = json.loads(line)
        payload = req["payload"]
        if req.get("protocol") != PROTOCOL:
            result = {"status": "PROTOCOL_ERROR", "error_class": "protocol_version"}
        else:
            source = payload["source"]
            channel = next(
                (
                    c["username"]
                    for c in channels
                    if str(c["id"]) == str(source.get("telegram_id"))
                ),
                source.get("channel_ref") or source.get("username"),
            )
            result = transport.call(
                "/__mock/telegram/" + payload["op"],
                {**payload, "channel": channel, "consumer": "social"},
            )
            if "messages" in result:
                result["messages"] = [
                    {
                        "external_message_id": r["id"],
                        "event_ts": r.get("event_ts"),
                        "author_id": r.get("author_id"),
                        "author_kind": r.get("author_kind"),
                        "text": r.get("text", ""),
                    }
                    for r in result["messages"]
                ]
        print(
            json.dumps(
                {
                    "protocol": PROTOCOL,
                    "request_id": req["request_id"],
                    "payload": result,
                }
            ),
            flush=True,
        )


if __name__ == "__main__":
    main()
