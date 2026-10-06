import asyncio
import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "adapters"))
from fixture_transport import Transport
from telegram_fixture import FixtureTelegramClient


class TransportFence(unittest.TestCase):
    def test_only_numeric_loopback_origins(self):
        for url in [
            "https://127.0.0.1:1",
            "http://example.com",
            "http://127.0.0.1:1/path",
            "http://127.0.0.1:1?x=1",
            "http://user:pass@127.0.0.1:1",
            "http://127.0.0.1:1#x",
        ]:
            with self.assertRaises(ValueError):
                Transport(url)
        Transport("http://127.0.0.1:1")


class ClientRace(unittest.IsolatedAsyncioTestCase):
    async def test_message_between_history_and_poll_is_not_skipped(self):
        client = FixtureTelegramClient("http://127.0.0.1:1")
        channel = {"id": 1001, "username": "FixtureCalls"}
        client.transport.call = lambda *args: [channel]

        async def call(op, payload):
            if op == "head":
                return {"status": "OK", "head_message_id": 10}
            if op == "updates":
                self.assertEqual(payload["after_message_id"], 10)
                return {
                    "status": "OK",
                    "messages": [
                        {
                            "id": 11,
                            "channel": "FixtureCalls",
                            "event_ts": datetime.now(timezone.utc).isoformat(),
                            "text": "new event",
                        }
                    ],
                }
            return {"status": "OK", "messages": []}

        client._call = call
        await client.start()
        seen = []

        async def callback(event):
            seen.append(event.message.id)
            await client.disconnect()

        client.add_event_handler(callback, SimpleNamespace(chats=None))
        await asyncio.wait_for(client.run_until_disconnected(), 1)
        self.assertEqual(seen, [11])


if __name__ == "__main__":
    unittest.main()
