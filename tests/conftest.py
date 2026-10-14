class FakeResponse:
    def __init__(self, payload=None, exc: Exception | None = None):
        self._payload = payload
        self._exc = exc

    async def json(self, content_type=None):
        if self._exc is not None:
            raise self._exc
        return self._payload

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc_info):
        return False


class FakeSession:
    """Stands in for aiohttp.ClientSession; returns queued responses in order."""

    def __init__(self, *responses: FakeResponse):
        self.responses = list(responses)
        self.calls: list[tuple[str, dict | None]] = []

    def get(self, url, params=None, timeout=None):
        self.calls.append((url, params))
        return self.responses.pop(0)


def ok(result) -> FakeResponse:
    return FakeResponse({"status": "OK", "result": result})


def failed(comment: str) -> FakeResponse:
    return FakeResponse({"status": "FAILED", "comment": comment})
