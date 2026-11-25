"""SMW-16/2048 v1 primitives from the closed-network reference."""
from hashlib import sha256, blake2s
from struct import pack
import re
def hex32(s): return isinstance(s,str) and bool(re.fullmatch(r"[0-9a-f]{64}",s))
MX10_WORK_ALGORITHM = '25cdf8f5acd57e409d8324c71c15b271314e4effd3ef2ca75ff2c897d2275d17'
VERSION = 10
N = 16
INNER = 2048
CERTIFICATE_BYTES = 105
CHALLENGE_DOMAIN = b'SVY-POW-MX10-CHALLENGE\0'
INPUT_DOMAIN = b'SVY-POW-MX10-INPUT\0'
COMMIT_DOMAIN = b'SVY-POW-MX10-COMMIT\0'
TICKET_DOMAIN = b'SVY-POW-MX10-TICKET\0'
MAP_ID = b'SVY-POW-MX10-MAP-I32-SUB-128\0'
WORKID_DOMAIN = b'SVY-POW-MX10-WORKID\0'
def challenge_for(genesis, pre_hash):
    if not (type(genesis) is str and genesis.startswith('0x') and hex32(genesis[2:]) and hex32(pre_hash)):
        raise ValueError('canonical chain and preseal hashes required')
    return sha256(CHALLENGE_DOMAIN + bytes([VERSION]) + bytes.fromhex(genesis[2:]) +
                  bytes.fromhex(pre_hash)).hexdigest()


def _fields(c):
    return c['challenge'] + pack('<Q', c['nonce']) + c['input_digest'] + c['output_digest']


def claimed_ticket(c):
    fields = _fields(c)
    commitment = sha256(COMMIT_DOMAIN + fields).digest()
    return int.from_bytes(sha256(TICKET_DOMAIN + fields + commitment).digest(), 'big')


def compute_certificate(challenge_hex, nonce):
    """Full MX10 recomputation with run constants n=16, inner=2048. Returns (cert_hex, ticket_hex)."""
    context = bytes.fromhex(challenge_hex)
    prefix = INPUT_DOMAIN + context + pack('<QII', nonce, N, INNER)
    stream = b''.join(blake2s(prefix + pack('<I', c)).digest() for c in range(2 * N * INNER // 32))
    mapped = [b - 128 for b in stream]
    rows = [mapped[i * INNER:(i + 1) * INNER] for i in range(N)]
    cols = [mapped[(N + i) * INNER:(N + i + 1) * INNER] for i in range(N)]
    in_hash = sha256(pack('<%di' % len(mapped), *mapped)).digest()
    out = []
    for r in rows:
        for c in cols:
            v = sum(x * y for x, y in zip(r, c))
            if not -(1 << 31) <= v < (1 << 31):
                raise OverflowError('i32 matrix result')
            out.append(v)
    out_hash = sha256(pack('<%di' % (N * N), *out)).digest()
    cert = dict(challenge=context, nonce=nonce, input_digest=in_hash, output_digest=out_hash)
    return (bytes([VERSION]) + _fields(cert)).hex(), f'{claimed_ticket(cert):064x}'
