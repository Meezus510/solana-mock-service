"""Telethon-compatible application boundary; no Telegram authentication/session."""

import asyncio
from datetime import datetime, timezone
from types import SimpleNamespace

from fixture_transport import Transport
from telethon import types
from telethon.errors import FloodWaitError


class FixtureTelegramClient:
    def __init__(self, origin=None):
        self.transport = Transport(origin)
        self.connected = False
        self.handlers = []
        self.cursors = {}
        self.ready = asyncio.Event()
        self.channels = []

    async def _call(self, op, payload):
        result = await asyncio.to_thread(
            self.transport.call,
            "/__mock/telegram/" + op,
            {**payload, "consumer": "signal"},
        )
        if result.get("status") == "FLOOD_WAIT":
            raise FloodWaitError(
                request=None, capture=int(result.get("flood_wait_seconds", 1))
            )
        if result.get("status") == "DISCONNECTED":
            self.connected = False
            raise ConnectionError("scripted fixture disconnect")
        if result.get("status") != "OK":
            raise RuntimeError("fixture Telegram provider error: " + str(result))
        return result

    async def start(self, **kwargs):
        self.channels = await asyncio.to_thread(
            self.transport.call, "/__mock/telegram/channels"
        )
        for c in self.channels:
            head = await self._call("head", {"channel": c["username"]})
            self.cursors[c["username"]] = head["head_message_id"]
        self.connected = True
        self.ready.set()
        return self

    async def get_me(self):
        return SimpleNamespace(id=1, username="fixture_account")

    def _channel(self, entity):
        if isinstance(entity, str):
            return next(
                c
                for c in self.channels
                if c["username"].lower() == entity.lstrip("@").lower()
            )
        return next(
            c
            for c in self.channels
            if c["id"] == getattr(entity, "id", getattr(entity, "channel_id", None))
        )

    async def get_entity(self, name):
        c = self._channel(name)
        return types.Channel(
            id=c["id"],
            title=c["username"],
            photo=types.ChatPhotoEmpty(),
            date=datetime.now(timezone.utc),
            username=c["username"],
            access_hash=c.get("access_hash", 1),
            broadcast=True,
        )

    async def get_input_entity(self, entity):
        c = self._channel(entity)
        return types.InputPeerChannel(c["id"], c.get("access_hash", 1))

    @staticmethod
    def _message(row):
        date = datetime.fromisoformat(row["event_ts"].replace("Z", "+00:00"))
        return SimpleNamespace(
            id=row["id"],
            date=date,
            message=row.get("text", ""),
            text=row.get("text", ""),
            raw_text=row.get("text", ""),
            entities=[],
            sender_id=row.get("author_id"),
            chat_id=row.get("channel"),
            out=False,
        )

    async def get_messages(self, entity, limit=100, min_id=0, max_id=None, **kwargs):
        c = self._channel(entity)
        if limit == 1 and min_id == 0 and max_id is None:
            head = await self._call("head", {"channel": c["username"]})
            maximum = head["head_message_id"]
            rows = await self._call(
                "page",
                {
                    "channel": c["username"],
                    "after_message_id": max(0, maximum - 1),
                    "limit": 1,
                },
            )
        else:
            rows = await self._call(
                "page",
                {
                    "channel": c["username"],
                    "after_message_id": min_id,
                    "max_id": max_id or 2**63 - 1,
                    "limit": limit,
                },
            )
        return [self._message(r) for r in reversed(rows["messages"])]

    async def iter_messages(self, entity, limit=100, min_id=0, reverse=False, **kwargs):
        rows = await self.get_messages(entity, limit=limit, min_id=min_id, **kwargs)
        for row in reversed(rows) if reverse else rows:
            yield row

    def add_event_handler(self, callback, builder):
        self.handlers.append((callback, builder))

    def remove_event_handler(self, callback, *args):
        self.handlers = [(cb, b) for cb, b in self.handlers if cb != callback]

    async def __call__(self, request):
        return SimpleNamespace(chats=[], users=[], updates=[])

    def is_connected(self):
        return self.connected

    async def disconnect(self):
        self.connected = False

    async def run_until_disconnected(self):
        # Establish provider heads once; production startup handles history.
        for c in self.channels:
            if c["username"] not in self.cursors:
                head = await self._call("head", {"channel": c["username"]})
                self.cursors[c["username"]] = head["head_message_id"]
        while self.connected:
            for c in self.channels:
                name = c["username"]
                result = await self._call(
                    "updates",
                    {
                        "channel": name,
                        "after_message_id": self.cursors[name],
                        "limit": 100,
                    },
                )
                for row in result["messages"]:
                    event = SimpleNamespace(
                        message=self._message(row),
                        chat=SimpleNamespace(username=name, title=name),
                        chat_id=-1000000000000 - c["id"],
                    )
                    for cb, builder in list(self.handlers):
                        chats = builder.chats
                        if chats is None or any(
                            getattr(x, "id", getattr(x, "channel_id", None)) == c["id"]
                            for x in chats
                        ):
                            await cb(event)
                    self.cursors[name] = row["id"]
            await asyncio.sleep(0.1)
