"""Loopback-only HTTP boundary shared by fixture adapters."""

import ipaddress
import json
import os
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise RuntimeError("fixture transport redirects are forbidden")


class Transport:
    def __init__(self, origin=None):
        self.origin = (origin or os.environ["TELEGRAM_FIXTURE_URL"]).rstrip("/")
        url = urlsplit(self.origin)
        if (
            url.scheme != "http"
            or not ipaddress.ip_address(url.hostname).is_loopback
            or url.username
            or url.password
            or url.path not in ("", "/")
            or url.query
            or url.fragment
        ):
            raise ValueError("fixture origin must be numeric HTTP loopback")
        self.opener = build_opener(ProxyHandler({}), NoRedirect())

    def call(self, path, payload=None):
        data = None if payload is None else json.dumps(payload).encode()
        request = Request(
            self.origin + path, data=data, headers={"Content-Type": "application/json"}
        )
        with self.opener.open(request, timeout=10) as response:
            return json.load(response)
