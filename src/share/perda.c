// Perda de pacote sem sudo, pras bancadas: carregado por LD_PRELOAD, descarta PERDA% dos
// datagramas RTP/RTCP que o processo envia, como um `netem loss` na saída. Só o RTP/RTCP
// (primeiro byte 128-191, RFC 7983): STUN, DTLS e DNS passam. O descarte finge sucesso.
//   cc -O2 -shared -fPIC -o perda.so src/share/perda.c
//   LD_PRELOAD=$PWD/perda.so PERDA=5 <comando>
// Os filhos herdam (o Chrome inteiro, por exemplo). Sem PERDA, não descarta nada. No fim,
// grava no stderr quantos descartou. PERDA_SEMENTE fixa a sequência.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <netinet/in.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <unistd.h>

static uint64_t limite;  // descarta quando o sorteio de 32 bits cai abaixo dele
static uint64_t semente;
static atomic_ulong vistos, descartados;
static ssize_t (*real_sendto)(int, const void *, size_t, int, const struct sockaddr *, socklen_t);
static ssize_t (*real_sendmsg)(int, const struct msghdr *, int);

__attribute__((constructor)) static void inicia(void) {
  real_sendto = dlsym(RTLD_NEXT, "sendto");
  real_sendmsg = dlsym(RTLD_NEXT, "sendmsg");
  const char *p = getenv("PERDA"), *s = getenv("PERDA_SEMENTE");
  double pct = p ? atof(p) : 0;
  limite = (uint64_t)(pct / 100.0 * 4294967296.0);
  semente = s ? strtoull(s, NULL, 10) : (uint64_t)getpid() * 0x9E3779B97F4A7C15ull;
}

__attribute__((destructor)) static void fim(void) {
  if (limite && vistos)
    fprintf(stderr, "perda.so pid %d: %lu de %lu pacotes RTP/RTCP descartados\n", getpid(),
            (unsigned long)descartados, (unsigned long)vistos);
}

// splitmix64 com estado por thread, pra não disputar trava no envio.
static uint32_t sorteio(void) {
  static __thread uint64_t x;
  if (!x) x = semente ^ ((uint64_t)gettid() << 32) ^ 1;
  uint64_t z = (x += 0x9E3779B97F4A7C15ull);
  z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
  z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
  return (uint32_t)((z ^ (z >> 31)) >> 32);
}

static int descarta(const void *buf, size_t len, const struct sockaddr *to) {
  if (!limite || !to || len == 0 || (((const uint8_t *)buf)[0] & 0xC0) != 0x80) return 0;
  in_port_t porta;
  if (to->sa_family == AF_INET) porta = ((const struct sockaddr_in *)to)->sin_port;
  else if (to->sa_family == AF_INET6) porta = ((const struct sockaddr_in6 *)to)->sin6_port;
  else return 0;
  if (ntohs(porta) == 53) return 0;
  atomic_fetch_add(&vistos, 1);
  if (sorteio() >= limite) return 0;
  atomic_fetch_add(&descartados, 1);
  return 1;
}

ssize_t sendto(int fd, const void *buf, size_t len, int flags, const struct sockaddr *to,
               socklen_t tolen) {
  if (descarta(buf, len, to)) return (ssize_t)len;
  return real_sendto(fd, buf, len, flags, to, tolen);
}

// O sendmsg com um iovec só, o outro caminho de um datagrama.
ssize_t sendmsg(int fd, const struct msghdr *msg, int flags) {
  if (msg->msg_iovlen == 1 &&
      descarta(msg->msg_iov[0].iov_base, msg->msg_iov[0].iov_len, msg->msg_name))
    return (ssize_t)msg->msg_iov[0].iov_len;
  return real_sendmsg(fd, msg, flags);
}
