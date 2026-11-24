// MX10 evaluation-only GPU path (CUDA). Bit-exact reimplementation of
// gpu_pow_mx10::compute (derive + matrix + digests). NOT the V2 kernel; does
// not share source with gpu_pow::mine(). Not wired into consensus/mining.
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <chrono>
#include <vector>
#include <string>
#include <unistd.h>

#define CK(x) do { cudaError_t e_ = (x); if (e_ != cudaSuccess) { fprintf(stderr, "CUDA %s at %s:%d\n", cudaGetErrorString(e_), __FILE__, __LINE__); exit(2);} } while (0)

static const int K_DIM = 2048;
__constant__ uint8_t c_challenge[32];
__constant__ uint32_t c_blake_iv[8] = {0x6A09E667u,0xBB67AE85u,0x3C6EF372u,0xA54FF53Au,0x510E527Fu,0x9B05688Cu,0x1F83D9ABu,0x5BE0CD19u};
__constant__ uint8_t c_sigma[10][16] = {
 {0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15},{14,10,4,8,9,15,13,6,1,12,0,2,11,7,5,3},
 {11,8,12,0,5,2,15,13,10,14,3,6,7,1,9,4},{7,9,3,1,13,12,11,14,2,6,5,10,4,0,15,8},
 {9,0,5,7,2,4,10,15,14,1,11,12,6,8,3,13},{2,12,6,10,0,11,8,3,4,13,7,5,15,14,1,9},
 {12,5,1,15,14,13,4,10,0,7,6,3,9,2,8,11},{13,11,7,14,12,1,3,9,5,0,15,4,8,6,2,10},
 {6,15,14,9,11,3,0,8,12,2,13,7,1,4,10,5},{10,2,8,4,7,6,1,5,15,11,9,14,3,12,13,0}};
__constant__ uint32_t c_sha_k[64] = {
0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2};

__device__ __forceinline__ uint32_t rotr(uint32_t x, int r) { return (x >> r) | (x << (32 - r)); }

__device__ void blake2s_compress(uint32_t h[8], const uint32_t m[16], uint32_t t, bool last) {
  uint32_t v[16];
  for (int i = 0; i < 8; i++) { v[i] = h[i]; v[i + 8] = c_blake_iv[i]; }
  v[12] ^= t; // t1 = 0
  if (last) v[14] = ~v[14];
#define G(a,b,c,d,x,y) { v[a]=v[a]+v[b]+x; v[d]=rotr(v[d]^v[a],16); v[c]=v[c]+v[d]; v[b]=rotr(v[b]^v[c],12); \
                          v[a]=v[a]+v[b]+y; v[d]=rotr(v[d]^v[a],8); v[c]=v[c]+v[d]; v[b]=rotr(v[b]^v[c],7); }
  for (int r = 0; r < 10; r++) {
    const uint8_t* s = c_sigma[r];
    G(0,4,8,12,m[s[0]],m[s[1]]); G(1,5,9,13,m[s[2]],m[s[3]]); G(2,6,10,14,m[s[4]],m[s[5]]); G(3,7,11,15,m[s[6]],m[s[7]]);
    G(0,5,10,15,m[s[8]],m[s[9]]); G(1,6,11,12,m[s[10]],m[s[11]]); G(2,7,8,13,m[s[12]],m[s[13]]); G(3,4,9,14,m[s[14]],m[s[15]]);
  }
#undef G
  for (int i = 0; i < 8; i++) h[i] ^= v[i] ^ v[i + 8];
}

// One thread per (nonce, counter). Message (71 bytes):
// "SVY-POW-MX10-INPUT\0"(19) || challenge(32) || nonce_le_u64 || n_le_u32 || 2048_le_u32 || counter_le_u32
// Output byte b mapped to i32(b)-128, stored as int8 (two's complement == b ^ 0x80).
__global__ void k_derive(int8_t* vals, uint64_t nonce_base, uint32_t n, uint32_t batch) {
  uint32_t per = 128u * n; // (2*n*2048)/32
  uint64_t gid = (uint64_t)blockIdx.x * blockDim.x + threadIdx.x;
  if (gid >= (uint64_t)per * batch) return;
  uint32_t idx = (uint32_t)(gid / per), counter = (uint32_t)(gid % per);
  uint64_t nonce = nonce_base + idx;
  uint8_t msg[128];
  const char dom[19] = {'S','V','Y','-','P','O','W','-','M','X','1','0','-','I','N','P','U','T',0};
  for (int i = 0; i < 128; i++) msg[i] = 0;
  for (int i = 0; i < 19; i++) msg[i] = (uint8_t)dom[i];
  for (int i = 0; i < 32; i++) msg[19 + i] = c_challenge[i];
  for (int i = 0; i < 8; i++) msg[51 + i] = (uint8_t)(nonce >> (8 * i));
  for (int i = 0; i < 4; i++) msg[59 + i] = (uint8_t)(n >> (8 * i));
  for (int i = 0; i < 4; i++) msg[63 + i] = (uint8_t)(2048u >> (8 * i));
  for (int i = 0; i < 4; i++) msg[67 + i] = (uint8_t)(counter >> (8 * i));
  uint32_t h[8];
  for (int i = 0; i < 8; i++) h[i] = c_blake_iv[i];
  h[0] ^= 0x01010020u; // digest 32, key 0, fanout 1, depth 1
  uint32_t m[16];
  for (int blk = 0; blk < 2; blk++) {
    for (int i = 0; i < 16; i++) {
      const uint8_t* p = msg + blk * 64 + 4 * i;
      m[i] = (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
    }
    blake2s_compress(h, m, blk == 0 ? 64u : 71u, blk == 1);
  }
  uint32_t* out = (uint32_t*)(vals + (size_t)idx * (2u * n * K_DIM) + (size_t)counter * 32);
  for (int i = 0; i < 8; i++) out[i] = h[i] ^ 0x80808080u; // LE bytes b -> b^0x80
}

// C[row][col] = sum_k A[row][k]*B[k][col]; A rows at vals[row*K], B cols at vals[(n+col)*K].
// Tiled 16x16 outputs per block, exact signed int8 dp4a with i32 accumulate.
__global__ void k_matmul(const int8_t* vals, int32_t* outc, uint32_t n) {
  __shared__ int As[16][65];
  __shared__ int Bs[16][65];
  uint32_t idx = blockIdx.z;
  const int* base = (const int*)(vals + (size_t)idx * (2u * n * K_DIM));
  int tx = threadIdx.x, ty = threadIdx.y;
  int row = blockIdx.y * 16 + ty, col = blockIdx.x * 16 + tx;
  const int WORDS = K_DIM / 4; // 512
  int acc = 0;
  for (int w0 = 0; w0 < WORDS; w0 += 64) {
    for (int j = tx; j < 64; j += 16) {
      As[ty][j] = base[(size_t)(blockIdx.y * 16 + ty) * WORDS + w0 + j];
      Bs[ty][j] = base[(size_t)(n + blockIdx.x * 16 + ty) * WORDS + w0 + j];
    }
    __syncthreads();
#pragma unroll 16
    for (int j = 0; j < 64; j++) acc = __dp4a(As[ty][j], Bs[tx][j], acc);
    __syncthreads();
  }
  outc[(size_t)idx * n * n + (size_t)row * n + col] = acc;
}

__device__ void sha256_compress(uint32_t st[8], const uint32_t w_in[16]) {
  uint32_t w[64];
  for (int i = 0; i < 16; i++) w[i] = w_in[i];
  for (int i = 16; i < 64; i++) {
    uint32_t s0 = rotr(w[i-15],7) ^ rotr(w[i-15],18) ^ (w[i-15] >> 3);
    uint32_t s1 = rotr(w[i-2],17) ^ rotr(w[i-2],19) ^ (w[i-2] >> 10);
    w[i] = w[i-16] + s0 + w[i-7] + s1;
  }
  uint32_t a=st[0],b=st[1],c=st[2],d=st[3],e=st[4],f=st[5],g=st[6],h=st[7];
  for (int i = 0; i < 64; i++) {
    uint32_t S1 = rotr(e,6) ^ rotr(e,11) ^ rotr(e,25);
    uint32_t ch = (e & f) ^ (~e & g);
    uint32_t t1 = h + S1 + ch + c_sha_k[i] + w[i];
    uint32_t S0 = rotr(a,2) ^ rotr(a,13) ^ rotr(a,22);
    uint32_t mj = (a & b) ^ (a & c) ^ (b & c);
    uint32_t t2 = S0 + mj;
    h=g; g=f; f=e; e=d+t1; d=c; c=b; b=a; a=t1+t2;
  }
  st[0]+=a; st[1]+=b; st[2]+=c; st[3]+=d; st[4]+=e; st[5]+=f; st[6]+=g; st[7]+=h;
}
__device__ void sha_init(uint32_t st[8]) {
  st[0]=0x6a09e667; st[1]=0xbb67ae85; st[2]=0x3c6ef372; st[3]=0xa54ff53a;
  st[4]=0x510e527f; st[5]=0x9b05688c; st[6]=0x1f83d9ab; st[7]=0x5be0cd19;
}
__device__ void sha_pad_final(uint32_t st[8], uint64_t total_bytes, uint8_t* out32) {
  uint32_t w[16];
  for (int i = 0; i < 16; i++) w[i] = 0;
  w[0] = 0x80000000u;
  uint64_t bits = total_bytes * 8;
  w[14] = (uint32_t)(bits >> 32); w[15] = (uint32_t)bits;
  sha256_compress(st, w);
  for (int i = 0; i < 8; i++) { out32[4*i]=st[i]>>24; out32[4*i+1]=st[i]>>16; out32[4*i+2]=st[i]>>8; out32[4*i+3]=st[i]; }
}

// SHA-256 for the two short MX10 ticket messages (at most 156 bytes).
__device__ void sha_short(const uint8_t* src, uint32_t len, uint8_t out32[32]) {
  uint32_t st[8], w[16]; sha_init(st);
  uint32_t blocks = (len + 9u + 63u) / 64u;
  for (uint32_t b = 0; b < blocks; b++) {
    for (int i = 0; i < 16; i++) w[i] = 0;
    for (uint32_t j = 0; j < 64; j++) {
      uint32_t pos = b * 64u + j; uint8_t v = 0;
      if (pos < len) v = src[pos];
      else if (pos == len) v = 0x80;
      else if (b == blocks - 1 && j >= 56) {
        uint64_t bits = (uint64_t)len * 8u;
        v = (uint8_t)(bits >> (8u * (63u - j)));
      }
      w[j >> 2] |= (uint32_t)v << (24 - 8 * (j & 3));
    }
    sha256_compress(st, w);
  }
  for (int i = 0; i < 8; i++) { out32[4*i]=st[i]>>24; out32[4*i+1]=st[i]>>16; out32[4*i+2]=st[i]>>8; out32[4*i+3]=st[i]; }
}

// ticket = SHA256(TICKET_DOMAIN || fields || SHA256(COMMIT_DOMAIN || fields))
// fields = challenge || nonce_le || input_digest || output_digest.
__global__ void k_tickets(const uint8_t* dig, uint8_t* tickets, uint64_t nonce_base, uint32_t batch) {
  uint32_t idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= batch) return;
  const char commit_dom[20] = {'S','V','Y','-','P','O','W','-','M','X','1','0','-','C','O','M','M','I','T',0};
  const char ticket_dom[20] = {'S','V','Y','-','P','O','W','-','M','X','1','0','-','T','I','C','K','E','T',0};
  uint8_t fields[104];
  for (int i = 0; i < 32; i++) fields[i] = c_challenge[i];
  uint64_t nonce = nonce_base + idx;
  for (int i = 0; i < 8; i++) fields[32+i] = (uint8_t)(nonce >> (8*i));
  for (int i = 0; i < 64; i++) fields[40+i] = dig[(size_t)idx*64+i];
  uint8_t first[124], commitment[32], second[156];
  for (int i = 0; i < 20; i++) first[i] = (uint8_t)commit_dom[i];
  for (int i = 0; i < 104; i++) first[20+i] = fields[i];
  sha_short(first, 124, commitment);
  for (int i = 0; i < 20; i++) second[i] = (uint8_t)ticket_dom[i];
  for (int i = 0; i < 104; i++) second[20+i] = fields[i];
  for (int i = 0; i < 32; i++) second[124+i] = commitment[i];
  sha_short(second, 156, tickets + (size_t)idx*32);
}

// One thread per nonce. input_digest = SHA-256 of all 2*n*2048 values as i32 LE;
// output_digest = SHA-256 of n*n i32 LE row-major. Both lengths are multiples of 64.
__global__ void k_digests(const int8_t* vals, const int32_t* outc, uint8_t* dig, uint32_t n, uint32_t batch) {
  uint32_t idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= batch) return;
  uint32_t st[8], w[16];
  sha_init(st);
  const uint4* v = (const uint4*)(vals + (size_t)idx * (2u * n * K_DIM));
  uint32_t blocks = (2u * n * K_DIM) / 16;
  for (uint32_t bI = 0; bI < blocks; bI++) {
    uint4 q = v[bI];
    uint32_t qs[4] = {q.x, q.y, q.z, q.w};
    for (int j = 0; j < 16; j++) {
      uint32_t byte = (qs[j >> 2] >> (8 * (j & 3))) & 0xff;
      uint32_t sx = (byte & 0x80) ? 0xffu : 0u;
      w[j] = (byte << 24) | (sx << 16) | (sx << 8) | sx; // BE word of i32 LE bytes
    }
    sha256_compress(st, w);
  }
  sha_pad_final(st, (uint64_t)2u * n * K_DIM * 4, dig + (size_t)idx * 64);
  sha_init(st);
  const uint32_t* c = (const uint32_t*)(outc + (size_t)idx * n * n);
  for (uint32_t bI = 0; bI < (n * n) / 16; bI++) {
    for (int j = 0; j < 16; j++) w[j] = __byte_perm(c[bI * 16 + j], 0, 0x0123);
    sha256_compress(st, w);
  }
  sha_pad_final(st, (uint64_t)n * n * 4, dig + (size_t)idx * 64 + 32);
}

struct Ctx { uint32_t n, cap; int8_t* vals; int32_t* outc; uint8_t* dig; uint8_t* tickets; };
static void ctx_init(Ctx& c, uint32_t n, uint32_t cap) {
  c.n = n; c.cap = cap;
  CK(cudaMalloc(&c.vals, (size_t)cap * 2 * n * K_DIM));
  CK(cudaMalloc(&c.outc, (size_t)cap * n * n * 4));
  CK(cudaMalloc(&c.dig, (size_t)cap * 64));
  CK(cudaMalloc(&c.tickets, (size_t)cap * 32));
}
// Full compute for nonces [base, base+batch): derive + matrix + digests, digests copied to host.
static void run_batch(Ctx& c, uint64_t base, uint32_t batch, uint8_t* host_dig) {
  uint64_t th = (uint64_t)128 * c.n * batch;
  k_derive<<<(unsigned)((th + 255) / 256), 256>>>(c.vals, base, c.n, batch);
  dim3 g(c.n / 16, c.n / 16, batch), b(16, 16);
  k_matmul<<<g, b>>>(c.vals, c.outc, c.n);
  k_digests<<<(batch + 63) / 64, 64>>>(c.vals, c.outc, c.dig, c.n, batch);
  k_tickets<<<(batch + 63) / 64, 64>>>(c.dig, c.tickets, base, batch);
  CK(cudaGetLastError());
  CK(cudaMemcpy(host_dig, c.dig, (size_t)batch * 64, cudaMemcpyDeviceToHost));
}
static void set_challenge(const uint8_t ch[32]) { CK(cudaMemcpyToSymbol(c_challenge, ch, 32)); }
static void hex(const uint8_t* p, int len, char* o) { for (int i = 0; i < len; i++) sprintf(o + 2 * i, "%02x", p[i]); }
static bool parse_hex32(const char* s, uint8_t out[32]) {
  if (strlen(s) != 64) return false;
  for (int i = 0; i < 32; i++) { unsigned x; if (sscanf(s + 2 * i, "%2x", &x) != 1) return false; out[i] = (uint8_t)x; }
  return true;
}

int main(int argc, char** argv) {
  if (argc < 2) { fprintf(stderr, "usage: vectors | batch N BASE COUNT CHALLENGEHEX | bench N BATCH SECONDS STOPFILE CHALLENGEHEX NONCEBASE | stream N BASE BATCH STOPFILE TARGETHEX CHALLENGEHEX\n"); return 1; }
  cudaDeviceProp p; CK(cudaGetDeviceProperties(&p, 0));
  char uuid[80]; { char* o = uuid; o += sprintf(o, "GPU-"); for (int i = 0; i < 16; i++) { o += sprintf(o, "%02x", (uint8_t)p.uuid.bytes[i]); if (i==3||i==5||i==7||i==9) o += sprintf(o, "-"); } }
  if (std::string(argv[1]) == "vectors") {
    // stdin lines: N NONCE CHALLENGEHEX ; stdout: N NONCE INPUTDIGEST OUTPUTDIGEST
    fprintf(stderr, "device %s %s\n", p.name, uuid);
    unsigned n; unsigned long long nonce; char chs[80];
    Ctx ctx[4]; bool inited[4] = {false,false,false,false};
    while (scanf("%u %llu %70s", &n, &nonce, chs) == 3) {
      int slot = n == 16 ? 0 : n == 32 ? 1 : n == 64 ? 2 : n == 128 ? 3 : -1;
      uint8_t ch[32];
      if (slot < 0 || !parse_hex32(chs, ch)) { fprintf(stderr, "bad line\n"); return 3; }
      if (!inited[slot]) { ctx_init(ctx[slot], n, 1); inited[slot] = true; }
      set_challenge(ch);
      uint8_t d[64]; run_batch(ctx[slot], nonce, 1, d);
      char a[65], b[65]; hex(d, 32, a); hex(d + 32, 32, b);
      printf("%u %llu %s %s\n", n, nonce, a, b);
    }
    return 0;
  }
  if (std::string(argv[1]) == "batch" && argc == 6) {
    // One batched launch over nonces [BASE, BASE+COUNT) (the same path bench uses).
    uint32_t n = atoi(argv[2]); uint64_t base = strtoull(argv[3], 0, 10); uint32_t count = atoi(argv[4]); uint8_t ch[32];
    if (!parse_hex32(argv[5], ch) || (n != 16 && n != 32 && n != 64 && n != 128) || count == 0) { fprintf(stderr, "bad args\n"); return 1; }
    Ctx c; ctx_init(c, n, count); set_challenge(ch);
    std::vector<uint8_t> d((size_t)count * 64); run_batch(c, base, count, d.data());
    for (uint32_t i = 0; i < count; i++) { char a[65], b[65]; hex(&d[(size_t)i * 64], 32, a); hex(&d[(size_t)i * 64 + 32], 32, b);
      printf("%u %llu %s %s\n", n, (unsigned long long)(base + i), a, b); }
    return 0;
  }
  if (std::string(argv[1]) == "bench" && argc == 8) {
    uint32_t n = atoi(argv[2]), batch = atoi(argv[3]); double secs = atof(argv[4]);
    const char* stopfile = argv[5]; uint8_t ch[32];
    if (!parse_hex32(argv[6], ch) || (n != 16 && n != 32 && n != 64 && n != 128) || secs > 55.0) { fprintf(stderr, "bad args\n"); return 1; }
    uint64_t base = strtoull(argv[7], 0, 10);
    Ctx c; ctx_init(c, n, batch); set_challenge(ch);
    std::vector<uint8_t> d((size_t)batch * 64);
    run_batch(c, base + (1ull << 40), batch, d.data()); // warm-up, not counted
    uint64_t iters = 0, nb = 0, last_base = base; bool stopped = false;
    auto t0 = std::chrono::steady_clock::now();
    double el = 0;
    while (true) {
      last_base = base + iters;
      run_batch(c, last_base, batch, d.data());
      iters += batch; nb++;
      el = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
      if (el >= secs) break;
      if (access(stopfile, F_OK) == 0) { stopped = true; break; }
    }
    char a[65], b[65]; hex(d.data(), 32, a); hex(d.data() + 32, 32, b);
    printf("{\"device\":\"%s\",\"uuid\":\"%s\",\"n\":%u,\"batch\":%u,\"batches\":%llu,\"iters\":%llu,\"window_seconds_actual\":%.9f,\"cs\":%.6f,\"stopped_by_stopfile\":%s,\"nonce_base\":%llu,\"sample_nonce\":%llu,\"sample_input_digest\":\"%s\",\"sample_output_digest\":\"%s\"}\n",
      p.name, uuid, n, batch, (unsigned long long)nb, (unsigned long long)iters, el, iters / el, stopped ? "true" : "false",
      (unsigned long long)base, (unsigned long long)last_base, a, b);
    return 0;
  }
  if (std::string(argv[1]) == "stream" && argc == 8) {
    uint32_t n = atoi(argv[2]); uint64_t base = strtoull(argv[3], 0, 10); uint32_t batch = atoi(argv[4]);
    const char* stopfile = argv[5]; uint8_t target[32], ch[32];
    if (n != 16 || batch == 0 || batch > 16384 || !parse_hex32(argv[6], target) || !parse_hex32(argv[7], ch)) { fprintf(stderr, "bad args\n"); return 1; }
    Ctx c; ctx_init(c, n, batch); set_challenge(ch);
    std::vector<uint8_t> d((size_t)batch * 64), tickets((size_t)batch * 32);
    uint64_t batches_done = 0, initial_base = base;
    while (access(stopfile, F_OK) != 0) {
      run_batch(c, base, batch, d.data());
      CK(cudaMemcpy(tickets.data(), c.tickets, (size_t)batch * 32, cudaMemcpyDeviceToHost));
      for (uint32_t i = 0; i < batch; i++) if (memcmp(&tickets[(size_t)i*32], target, 32) <= 0) {
        char a[65], b[65], t[65]; hex(&d[(size_t)i*64],32,a); hex(&d[(size_t)i*64+32],32,b); hex(&tickets[(size_t)i*32],32,t);
        printf("%llu %s %s %s\n", (unsigned long long)(base+i), a, b, t);
      }
      base += batch; batches_done++;
      // Frequent progress makes short PoW rounds report real work accurately;
      // the old 262,144-nonce interval discarded a large partial tail whenever
      // another worker found the block first.
      if ((batches_done & 15u) == 0) printf("@ %llu\n", (unsigned long long)(base - initial_base));
      fflush(stdout);
      if (base < batch) { fprintf(stderr, "nonce range exhausted\n"); return 4; }
    }
    return 0;
  }
  fprintf(stderr, "bad mode\n"); return 1;
}
