"""Select a TLS endpoint. Custom pools use the operating system trust store."""
import os
from urllib.parse import urlsplit


def endpoint(root):
    url = urlsplit(os.environ.get("SMW_POOL_URL", "tls://boot3.solvyren.com:18444"))
    if (url.scheme != "tls" or not url.hostname or url.username is not None
            or url.password is not None or url.path or url.query or url.fragment
            or not url.port):
        raise ValueError("Expected tls://hostname:port")
    pins = {"boot2.solvyren.com": "boot2.pem", "boot3.solvyren.com": "pool.pem"}
    pin = pins.get(url.hostname)
    return url.hostname, url.port, str(root / pin) if pin else None
