#!/usr/bin/env bash
# concurrency-scaling.sh - does the guest's FUSE path scale with concurrency?
#
# Question this answers: with N guest threads each doing independent small-file
# FUSE work, does aggregate ops/s rise with N, or does it flatten at the single
# virtio-fs server thread (`fs worker`) in the VMM? Flat + a saturated worker =
# server serialisation (a pool / io_uring would help). Flat + an idle worker =
# the guest or the per-request round trip is the limit (no server-side change
# can help).
#
# ONE guest boot, two filesystems inside it, alternating rounds so host drift
# hits both arms:
#
#   /workspace/tree   the single virtio-fs root share (1 request queue, 1 worker)
#   /dev/shm/tree     the same tree on guest tmpfs - the no-FUSE control
#
# Workload modes (one cell = one invocation of the in-guest C program):
#   open      openat(O_RDONLY) + close                 FUSE_OPEN + FUSE_RELEASE
#   read      open + fadvise(DONTNEED) + read + close  ... + a real FUSE_READ
#   stat      statx(AT_STATX_FORCE_SYNC)               forces a FUSE_GETATTR
#   statwarm  statx(flags=0)                           guest attr cache; control
#   readdir   opendir/full readdir/closedir            FUSE_READDIRPLUS batches
#
# cang's share is created with attr_timeout=entry_timeout=5 s (LinuxComplete
# semantics), so a repeated statx/open of a small file set is *cache-served*:
# statwarm exists to show that, and `stat`/`read` defeat it explicitly. Without
# that, a concurrency table measures the guest VFS and nothing else.
#
# While the payload runs, the VMM's `fs worker` thread is sampled at 4 Hz
# (state, wchan, utime+stime, schedstat runtime + runqueue wait, voluntary
# context switches) and the vCPU threads' ticks are summed. The fs pool
# threads (`fs work N`, the pool built by the libkrun fork's
# `krun_fs_device_set_worker_threads`) are summed the same way into their own
# columns, so a pool-armed run can say who did the work: a dispatcher that got
# cheaper while the pool did the rest is the pool working. The fields are read
# from after the LAST ')' of /proc/<pid>/task/<tid>/stat on purpose: the comm is
# literally "fs worker" and a naive awk field index silently reads the wrong
# fields (that bug cost this map a headline number).
#
# Usage: concurrency-scaling.sh [--files N] [--size BYTES] [--rounds N]
#                               [--threads "1 4 16 32"] [--modes LIST]
#                               [--cell SECONDS] [--bench-root DIR]
#
# Artifacts under <bench-root>/runs/concurrency-<stamp>/ :
#   tree/             the prepared file set (mounted as /workspace/tree)
#   results/cells.txt one CELL line per measured cell, with guest begin/end clock
#   samples.txt       the 4 Hz fs-worker / vCPU sampler capture
#   cang-cli.log      the cang CLI's own log (guest stdout lands here)
#   table.txt         the printed table
#
# Env: CANG (default <repo>/result/bin/cang), IMAGE, BENCH_ROOT, VM_TIMEOUT,
#      SAMPLE_INTERVAL (default 0.25 = 4 Hz).
set -uo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cang=${CANG:-$repo/result/bin/cang}
image=${IMAGE:-ghcr.io/zeroqn/cang:latest}
bench_root=${BENCH_ROOT:-/home/dev/cang-bench}
files=${FILES:-4096}
fsize=${SIZE:-4096}
threads_list=${THREADS:-1 4 16 32}
modes=${MODES:-open read stat statwarm readdirplus}
rounds=${ROUNDS:-3}
cell_secs=${CELL_SECS:-2.0}
sample_iv=${SAMPLE_INTERVAL:-0.25}
vm_timeout=${VM_TIMEOUT:-900}

while [ $# -gt 0 ]; do
    case $1 in
        --files) files=$2; shift 2 ;;
        --files=*) files=${1#*=}; shift ;;
        --size) fsize=$2; shift 2 ;;
        --size=*) fsize=${1#*=}; shift ;;
        --rounds) rounds=$2; shift 2 ;;
        --rounds=*) rounds=${1#*=}; shift ;;
        --threads) threads_list=$2; shift 2 ;;
        --threads=*) threads_list=${1#*=}; shift ;;
        --modes) modes=$2; shift 2 ;;
        --modes=*) modes=${1#*=}; shift ;;
        --cell) cell_secs=$2; shift 2 ;;
        --cell=*) cell_secs=${1#*=}; shift ;;
        --bench-root) bench_root=$2; shift 2 ;;
        --bench-root=*) bench_root=${1#*=}; shift ;;
        -h|--help) sed -n '2,48p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "concurrency-scaling.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

[ -x "$cang" ] || { echo "concurrency-scaling.sh: no cang at $cang (nix build .#cang)" >&2; exit 2; }

# A workspace that already holds a running task makes cang launch nothing at
# all (exit 0, no VM). Every run gets a fresh one, but say so if the host has a
# VM already: a co-tenant distorts the numbers, it is not something to chase.
if "$cang" ps 2>/dev/null | awk 'NR>1 && $3 ~ /^running|^attached/ {print; found=1} END{exit !found}'; then
    echo "concurrency-scaling.sh: WARNING - another cang task is running; numbers may carry its load" >&2
fi

run=$bench_root/runs/concurrency-$(date +%Y%m%d-%H%M%S)
mkdir -p "$run/tree" "$run/results"
echo "run dir: $run"

# ---------------------------------------------------------------- the tree ---
# One flat directory of ${files} files of ${fsize} bytes: the "thousands of
# small sources read once" shape, and small enough that the copy to /dev/shm is
# instant while still keeping the readdirplus scan expensive.
if [ ! -s "$run/tree/.prepared" ]; then
    filler=$(printf '%*s' "$fsize" '' | tr ' ' 'x')
    i=0
    while [ $i -lt "$files" ]; do
        printf '%05d %s\n' "$i" "${filler:0:$((fsize - 6))}" >"$run/tree/f$(printf '%05d' "$i").dat"
        i=$((i + 1))
    done
    : >"$run/tree/.prepared"
fi

cat >"$run/spec.env" <<SPEC
MODES="$modes"
THREADS="$threads_list"
ROUNDS=$rounds
SECS=$cell_secs
FILES=$files
SPEC

# ------------------------------------------------------------ guest payload --
# Everything the guest writes lands in /workspace (the share under test) so the
# host can read it afterwards: this cang build may not forward the payload's
# stdout or its exit status.
cat >"$run/payload.sh" <<'PAYLOAD'
#!/bin/sh
# In-guest driver: compile the workload, copy the tree to /dev/shm, then run the
# cell matrix with the two filesystems alternating between rounds.
set -u
WS=/workspace
RES=$WS/results
. "$WS/spec.env"
mkdir -p "$RES"
exec >>"$RES/payload.log" 2>&1
echo "payload start $(date +%s.%N)"
echo "uname: $(uname -a)"
echo "nproc: $(nproc)"

# The bench binary must live on an exec-permitted filesystem: /dev/shm is
# mounted noexec in this image, /workspace (virtiofs) allows exec but executing
# page faults through FUSE would pollute the virtiofs arm, so prefer a tmpfs.
bin=""
for d in /tmp /run /var/tmp "$WS/.bin"; do
    mkdir -p "$d" 2>/dev/null || continue
    cp "$WS/bench.c" "$d/bench.c" 2>/dev/null || echo "note: cannot write $d"
    gcc -O2 -pthread -o "$d/cang-bench" "$d/bench.c" 2>"$d/build.err"
    echo "build in $d rc=$? $(tail -3 "$d/build.err" 2>/dev/null)"
    [ -x "$d/cang-bench" ] || continue
    "$d/cang-bench" --probe >/dev/null 2>&1
    rc=$?
    echo "exec probe $d/cang-bench rc=$rc"
    if [ "$rc" -ne 126 ] && [ "$rc" -ne 127 ]; then bin="$d/cang-bench"; break; fi
done
if [ -z "$bin" ]; then
    echo "FATAL: no executable bench binary"; echo FATAL >"$RES/DONE"; exit 1
fi
echo "bench binary: $bin"
echo "workspace tree: $(ls /workspace/tree | wc -l) entries"
"$bin" -d /workspace/tree -m open -t 1 -s 0.2 -n 64 >/dev/null 2>&1

# The same tree on tmpfs: the no-FUSE control arm.
rm -rf /dev/shm/tree
cp -r "$WS/tree" /dev/shm/tree || { echo "FATAL: cp to /dev/shm failed"; echo FATAL >"$RES/DONE"; exit 1; }
rm -f /dev/shm/tree/.prepared
echo "shm tree: $(ls /dev/shm/tree | wc -l) entries, df: $(df -h /dev/shm | tail -1)"

# Warm-up pass: allocate per-thread state, populate the page cache and the FUSE
# attribute/entry caches, so the first measured cell is not the odd one out.
for fs_dir in "$WS/tree" /dev/shm/tree; do
    for mode in open read stat statwarm readdirplus; do
        "$bin" -d "$fs_dir" -m "$mode" -t 1 -s 0.3 -n 256 >/dev/null 2>&1
    done
done
echo "warm-up done $(date +%s.%N)"

# Clock sync ping #1: the content is the guest clock, the mtime the host's.
date +%s.%N >"$WS/sync-start"

i=0
round=1
while [ "$round" -le "$ROUNDS" ]; do
    if [ $((round % 2)) -eq 1 ]; then order="virtiofs shm"; else order="shm virtiofs"; fi
    for fs in $order; do
        case $fs in
            virtiofs) dir="$WS/tree" ;;
            shm) dir=/dev/shm/tree ;;
        esac
        for mode in $MODES; do
            for th in $THREADS; do
                beg=$(date +%s.%N)
                out=$(timeout 120 "$bin" -d "$dir" -m "$mode" -t "$th" -s "$SECS" 2>>"$RES/bench.err")
                end=$(date +%s.%N)
                echo "CELL round=$round fs=$fs mode=$mode threads=$th begin=$beg end=$end $out" >>"$RES/cells.txt"
            done
        done
    done
    round=$((round + 1))
done

date +%s.%N >"$WS/sync-end"
echo "payload done $(date +%s.%N)"
echo DONE >"$RES/DONE"
PAYLOAD
chmod +x "$run/payload.sh"

cat >"$run/bench.c" <<'BENCH_C'
/* cang concurrency-scaling probe workload - one measured cell per invocation.
 *
 *   cang-bench -d DIR -m MODE -t THREADS -s SECONDS [-n NFILES]
 *
 * Modes:
 *   open      openat(O_RDONLY) + close                        FUSE_OPEN + RELEASE
 *   read      open + fadvise(POSIX_FADV_DONTNEED) + read + close   + FUSE_READ
 *   stat      statx(AT_STATX_FORCE_SYNC, BASIC_STATS)          FUSE_GETATTR
 *   statwarm  statx(flags=0)                        guest attribute cache (control)
 *   readdir   opendir + full readdir + closedir                FUSE_READDIRPLUS
 *
 * Each thread walks the file list with a stride of THREADS so the threads touch
 * disjoint files, until its own deadline. Prints one parseable line:
 *   ops=.. secs=.. ops_s=.. p50_us=.. p99_us=.. cpu_pct=.. entries=.. errs=..
 * cpu_pct is the sum of the worker threads' CPU time as a percentage of one
 * core, i.e. how much guest CPU the arm actually burned.
 */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#ifndef AT_STATX_FORCE_SYNC
#define AT_STATX_FORCE_SYNC 0x2000
#endif

enum { M_OPEN, M_READ, M_STAT, M_STATWARM, M_READDIR, M_LAST };
static const char *mode_names[] = { "open", "read", "stat", "statwarm", "readdirplus" };

#define NBUCK 2048		/* 1 us buckets below 1 ms, log2 above */

static const char *g_dir;
static char **g_paths;
static int g_npaths;
static int g_mode = -1;
static int g_nthreads = 1;
static double g_secs = 1.0;
static double g_deadline;

static double now_mono(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (double)ts.tv_sec + ts.tv_nsec / 1e9;
}

static double now_thread_cpu(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ts);
	return (double)ts.tv_sec + ts.tv_nsec / 1e9;
}

static inline int bucket_of(uint64_t ns)
{
	uint64_t us = ns / 1000;
	if (us < 1024)
		return (int)us;
	int b = 1024;
	uint64_t v = us >> 10;
	while (v > 1 && b < NBUCK - 1) {
		v >>= 1;
		b++;
	}
	return b;
}

static inline double bucket_upper_us(int b)
{
	if (b < 1024)
		return (double)b + 1.0;
	return (double)(1024ULL << (b - 1023));
}

static double quantile_us(const uint32_t *hist, uint64_t total, double q)
{
	uint64_t want = (uint64_t)(total * q);
	if (want < 1)
		want = 1;
	uint64_t cum = 0;
	for (int b = 0; b < NBUCK; b++) {
		cum += hist[b];
		if (cum >= want)
			return bucket_upper_us(b);
	}
	return bucket_upper_us(NBUCK - 1);
}


/* Guest-wide CPU and interrupt counters from /proc/stat, so a cell can say how
 * much CPU the *guest* thinks it burned (compare with the host's vCPU thread
 * ticks: guest total << host ticks means the time went to the VMM, not the
 * guest). busy = all jiffies minus idle+iowait, summed over every CPU. */
static void guest_cpu_counters(uint64_t *busy, uint64_t *intr)
{
	*busy = 0;
	*intr = 0;
	FILE *f = fopen("/proc/stat", "r");
	if (!f)
		return;
	char line[512];
	while (fgets(line, sizeof(line), f)) {
		if (strncmp(line, "cpu ", 4) == 0) {
			unsigned long long v[10] = { 0 };
			sscanf(line + 4, "%llu %llu %llu %llu %llu %llu %llu %llu %llu %llu",
			       &v[0], &v[1], &v[2], &v[3], &v[4], &v[5], &v[6], &v[7], &v[8], &v[9]);
			unsigned long long total = 0;
			for (int i = 0; i < 8; i++)
				total += v[i];
			*busy = total - (v[3] + v[4]);
		} else if (strncmp(line, "intr ", 5) == 0) {
			unsigned long long n = 0;
			sscanf(line + 5, "%llu", &n);
			*intr = n;
		}
	}
	fclose(f);
}

typedef struct {
	int id;
	uint64_t ops;
	uint64_t entries;
	uint64_t errs;
	double cpu_s;
	uint32_t hist[NBUCK];
	char pad[64];
} worker_t;

static worker_t *g_workers;

static void *worker(void *arg)
{
	worker_t *w = arg;
	double cpu0 = now_thread_cpu();
	char buf[8192];
	int npaths = g_npaths;

	if (g_mode == M_READDIR) {
		while (now_mono() < g_deadline) {
			double a = now_mono();
			DIR *d = opendir(g_dir);
			if (!d) {
				w->errs++;
				continue;
			}
			struct dirent *e;
			while ((e = readdir(d)) != NULL)
				w->entries++;
			closedir(d);
			double b = now_mono();
			w->hist[bucket_of((uint64_t)((b - a) * 1e9))]++;
			w->ops++;
		}
		w->cpu_s = now_thread_cpu() - cpu0;
		return NULL;
	}

	int i = w->id % npaths;
	while (1) {
		double a = now_mono();
		if (a >= g_deadline)
			break;
		const char *p = g_paths[i];
		switch (g_mode) {
		case M_OPEN: {
			int fd = openat(AT_FDCWD, p, O_RDONLY);
			if (fd < 0)
				w->errs++;
			else
				close(fd);
			break;
		}
		case M_READ: {
			int fd = openat(AT_FDCWD, p, O_RDONLY);
			if (fd < 0) {
				w->errs++;
				break;
			}
			/* Drop the page cache for this file so the read really
			 * reaches the server: without this a repeated read of a
			 * small file set is a guest page-cache hit. */
			posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED);
			ssize_t n = read(fd, buf, sizeof(buf));
			if (n <= 0)
				w->errs++;
			close(fd);
			break;
		}
		case M_STAT:
		case M_STATWARM: {
			struct statx stx;
			int flags = (g_mode == M_STAT) ? AT_STATX_FORCE_SYNC : 0;
			if (statx(AT_FDCWD, p, flags, STATX_BASIC_STATS, &stx) != 0)
				w->errs++;
			break;
		}
		default:
			w->errs++;
			break;
		}
		double b = now_mono();
		w->hist[bucket_of((uint64_t)((b - a) * 1e9))]++;
		w->ops++;
		i += g_nthreads;
		if (i >= npaths)
			i -= npaths;
	}
	w->cpu_s = now_thread_cpu() - cpu0;
	return NULL;
}

static int cmp_names(const void *a, const void *b)
{
	return strcmp(*(char *const *)a, *(char *const *)b);
}

int main(int argc, char **argv)
{
	const char *dir = NULL;
	int opt;

	for (int i = 1; i < argc; i++)
		if (strcmp(argv[i], "--probe") == 0) {
			printf("probe ok\n");
			return 0;
		}

	while ((opt = getopt(argc, argv, "d:m:t:s:n:")) != -1) {
		switch (opt) {
		case 'd': dir = optarg; break;
		case 'm':
			g_mode = -1;
			for (int i = 0; i < M_LAST; i++)
				if (strcmp(optarg, mode_names[i]) == 0)
					g_mode = i;
			if (g_mode < 0) {
				fprintf(stderr, "unknown mode %s\n", optarg);
				return 2;
			}
			break;
		case 't': g_nthreads = atoi(optarg); break;
		case 's': g_secs = atof(optarg); break;
		case 'n': break;	/* accepted for symmetry, unused */
		default: return 2;
		}
	}
	if (!dir || g_mode < 0 || g_nthreads < 1) {
		fprintf(stderr, "usage: %s -d DIR -m MODE -t N -s SECS\n", argv[0]);
		return 2;
	}
	g_dir = dir;

	/* Enumerate the tree once, sorted, so every invocation walks the same
	 * order (the file set is the one thing the two arms must share). */
	DIR *d = opendir(dir);
	if (!d) {
		perror("opendir");
		return 1;
	}
	size_t cap = 0;
	struct dirent *e;
	while ((e = readdir(d)) != NULL) {
		if (e->d_name[0] == '.')
			continue;
		if ((size_t)g_npaths == cap) {
			cap = cap ? cap * 2 : 4096;
			g_paths = realloc(g_paths, cap * sizeof(*g_paths));
		}
		size_t len = strlen(dir) + strlen(e->d_name) + 2;
		g_paths[g_npaths] = malloc(len);
		snprintf(g_paths[g_npaths], len, "%s/%s", dir, e->d_name);
		g_npaths++;
	}
	closedir(d);
	if (g_npaths == 0) {
		fprintf(stderr, "no files in %s\n", dir);
		return 1;
	}
	qsort(g_paths, g_npaths, sizeof(*g_paths), cmp_names);

	uint64_t gbusy0 = 0, gintr0 = 0, gbusy1 = 0, gintr1 = 0, hz = (uint64_t)sysconf(_SC_CLK_TCK);
	if (hz == 0)
		hz = 100;
	guest_cpu_counters(&gbusy0, &gintr0);

	g_workers = calloc(g_nthreads, sizeof(*g_workers));
	pthread_t *th = calloc(g_nthreads, sizeof(*th));
	g_deadline = now_mono() + g_secs;
	double wall0 = now_mono();
	for (int i = 0; i < g_nthreads; i++) {
		g_workers[i].id = i;
		pthread_create(&th[i], NULL, worker, &g_workers[i]);
	}
	uint64_t ops = 0, entries = 0, errs = 0;
	uint32_t hist[NBUCK];
	memset(hist, 0, sizeof(hist));
	double cpu = 0.0;
	for (int i = 0; i < g_nthreads; i++) {
		pthread_join(th[i], NULL);
		ops += g_workers[i].ops;
		entries += g_workers[i].entries;
		errs += g_workers[i].errs;
		cpu += g_workers[i].cpu_s;
		for (int b = 0; b < NBUCK; b++)
			hist[b] += g_workers[i].hist[b];
	}
	double wall = now_mono() - wall0;
	guest_cpu_counters(&gbusy1, &gintr1);
	printf("ops=%llu secs=%.3f ops_s=%.0f p50_us=%.1f p99_us=%.1f cpu_pct=%.0f entries=%llu errs=%llu gcpu_pct=%.0f intr=%llu\n",
	       (unsigned long long)ops, wall, wall > 0 ? ops / wall : 0.0,
	       quantile_us(hist, ops, 0.50), quantile_us(hist, ops, 0.99),
	       wall > 0 ? cpu * 100.0 / wall : 0.0,
	       (unsigned long long)entries, (unsigned long long)errs,
	       (wall > 0 && gbusy1 >= gbusy0) ? (gbusy1 - gbusy0) * 100.0 / (hz * wall) : 0.0,
	       (unsigned long long)(gintr1 >= gintr0 ? gintr1 - gintr0 : 0));
	return 0;
}
BENCH_C

# ------------------------------------------------------------- the sampler ---
has_fs_worker_task()
{
    local t c
    for t in /proc/"$1"/task/*/comm; do
        [ -r "$t" ] || continue
        IFS= read -r c <"$t" || continue
        [ "$c" = "fs worker" ] && return 0
    done
    return 1
}

sampler_tick()
{
    local pid=$1 ts=$2 fsrc_tid="" fs_state="?" fs_ticks=0 fs_run=0 fs_wait=0 fs_vcs=0 wchan=0
    local pool_ticks=0 pool_run=0 pool_n=0
    local vcpu=0 vlist="" t c line rest state ut st run_ns wait_ns l
    for t in /proc/"$pid"/task/*; do
        [ -r "$t/stat" ] || continue
        IFS= read -r line <"$t/stat" || continue
        IFS= read -r c <"$t/comm" 2>/dev/null || c=""
        rest=${line##*)}		# strip through the LAST ')': comm has spaces
        # shellcheck disable=SC2086
        set -- $rest			# $1 = state, ${12} = utime, ${13} = stime
        state=$1
        ut=${12}
        st=${13}
        case $c in
        "fs worker")
            fsrc_tid=${t##*/}
            fs_state=$state
            fs_ticks=$((ut + st))
            # schedstat: runtime_ns runqueue_wait_ns timeslices - split on IFS,
            # so no "IFS=" here.
            read -r run_ns wait_ns _ <"$t/schedstat" 2>/dev/null || { run_ns=0; wait_ns=0; }
            fs_run=${run_ns:-0}
            fs_wait=${wait_ns:-0}
            # NOTE: /proc/<tid>/wchan has no trailing newline, so `read`
            # returns 1 at EOF *after* setting the variable - `|| l=0` would
            # discard every value and report wchan 0 for a sleeping thread.
            l=""
            IFS= read -r l <"$t/wchan" 2>/dev/null
            [ -z "$l" ] && l=0
            wchan=$l
            # Keep the last good value: reading the whole status file in a
            # `while read` loop returns 0 for ~1 sample in 5 under load (the
            # read is not atomic against a task in flux), and a cumulative
            # counter that suddenly reads 0 destroys the vcsw delta.
            v=${fs_vcs:-0}
            fs_vcs=$(awk '/^voluntary_ctxt_switches:/{print $2; f=1} END{exit !f}' \
                "$t/status" 2>/dev/null) || fs_vcs=$v
            ;;
        # Pool threads are named `fs work N`; the exact-match arm above is the
        # dispatcher, and `fs work ` (with the space) is deliberately not a
        # prefix of `fs worker`.
        "fs work "*)
            pool_ticks=$((pool_ticks + ut + st))
            pool_n=$((pool_n + 1))
            read -r prun_ns _ <"$t/schedstat" 2>/dev/null && pool_run=$((pool_run + ${prun_ns:-0}))
            ;;
        fc_vcpu*)
            vcpu=$((ut + st))
            vlist="${vlist:+$vlist,}$vcpu"
            ;;
        esac
    done
    [ -z "$vlist" ] && vlist=-
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$ts" "$fs_state" "${wchan:-0}" "$fs_ticks" "$fs_run" "$fs_wait" "$fs_vcs" "$vlist" "${fsrc_tid:-none}" \
        "$pool_ticks" "$pool_run" "$pool_n"
}

run_sampler()
{
    local out=$1 base=$2 pid="" waited=0
    : >"$out"
    while :; do
        pid=$(for d in /proc/[0-9]*; do
                  p=${d#/proc/}
                  grep -qx "$p" "$base" 2>/dev/null && continue
                  has_fs_worker_task "$p" && { echo "$p"; break; }
              done)
        [ -n "$pid" ] && break
        waited=$((waited + 1))
        if [ $waited -gt 900 ]; then
            echo "# no VMM with an 'fs worker' thread appeared" >>"$out"
            return 1
        fi
        sleep 0.2
    done
    echo "# vmm pid=$pid threads=$(ls -d /proc/"$pid"/task/* | wc -l) at $(date +%s.%N)" >>"$out"
    while [ -d "/proc/$pid" ]; do
        sampler_tick "$pid" "$(date +%s.%N)" >>"$out"
        sleep "$sample_iv"
    done
    echo "# vmm gone at $(date +%s.%N)" >>"$out"
}

# Record which VMMs exist before we start, so the sampler picks *our* fs worker
# even when another session is running its own VM on this host.
baseline=$run/baseline-pids.txt
: >"$baseline"
for d in /proc/[0-9]*; do
    has_fs_worker_task "${d#/proc/}" && echo "${d#/proc/}" >>"$baseline"
done
[ -s "$baseline" ] && echo "note: pre-existing VMM(s) $(tr '\n' ' ' <"$baseline") have an fs worker; ours is the new one"

# --------------------------------------------------------------- the arm -----
echo "launching: cang --image $image -- sh /workspace/payload.sh"
( cd "$run" && timeout "$vm_timeout" "$cang" --image "$image" -- sh /workspace/payload.sh ) \
    >"$run/cang-cli.log" 2>&1 &
cli=$!
run_sampler "$run/samples.txt" "$baseline" &
sampler=$!
wait $cli
cli_rc=$?
kill "$sampler" 2>/dev/null
wait "$sampler" 2>/dev/null
echo "cang CLI exit=$cli_rc; cells: $(grep -c '^CELL ' "$run/results/cells.txt" 2>/dev/null || echo 0)"

if ! grep -q '^DONE' "$run/results/DONE" 2>/dev/null; then
    echo "concurrency-scaling.sh: the payload did not finish; tail of the guest log:" >&2
    tail -20 "$run/results/payload.log" 2>/dev/null >&2 || true
    tail -20 "$run/cang-cli.log" >&2
fi

# ---------------------------------------------------------------- analysis ---
offset=$(awk -v g="$(cat "$run/sync-start" 2>/dev/null || echo 0)" \
             -v h="$(date -r "$run/sync-start" +%s.%N 2>/dev/null || echo 0)" \
             'BEGIN { printf "%.3f", h - g }')
offset_end=$(awk -v g="$(cat "$run/sync-end" 2>/dev/null || echo 0)" \
                 -v h="$(date -r "$run/sync-end" +%s.%N 2>/dev/null || echo 0)" \
                 'BEGIN { printf "%.3f", h - g }')

# cells.tsv: one row per measured cell, guest clock shifted onto the host clock.
awk -v off="$offset" '
    /^CELL / {
        delete v
        for (i = 1; i <= NF; i++) {
            n = split($i, kv, "=")
            if (n == 2) v[kv[1]] = kv[2]
        }
        printf "%s\t%s\t%s\t%.1f\t%.3f\t%.0f\t%.1f\t%.1f\t%.0f\t%s\t%s\t%s\t%s\t%s\n",
            v["fs"], v["mode"], v["threads"], v["begin"] + off, v["end"] + off,
            v["ops_s"], v["p50_us"], v["p99_us"], v["cpu_pct"], v["ops"],
            v["round"], v["secs"], v["gcpu_pct"], v["intr"]
    }' "$run/results/cells.txt" >"$run/cells.tsv"

# percell.tsv: the sampler's samples attributed to the cell whose window holds
# them, with the fs worker's runtime, state histogram and "parked in the event
# loop" share. Sample fields: ts state wchan ticks run_ns wait_ns vcsw
# vcpu_ticks tid
awk -F'\t' '
    NR == FNR {
        if ($1 ~ /^#/) next
        nc++
        seg[nc, "beg"] = $4 + 0; seg[nc, "end"] = $5 + 0
        fs[nc] = $1; md[nc] = $2; th[nc] = $3
        next
    }
    /^#/ { next }
    {
        ts = $1 + 0
        for (i = 1; i <= nc; i++) {
            if (ts >= seg[i, "beg"] && ts <= seg[i, "end"]) {
                if (!(i in s0)) {
                    s0[i] = $5; t0[i] = $4; r0[i] = $6; v0[i] = $8
                    p0[i] = $11; pn[i] = $12; tb[i] = ts
                }
                s1[i] = $5; t1[i] = $4; r1[i] = $6; v1[i] = $8; te[i] = ts
                p1[i] = $11; pnl[i] = $12
                n[i]++
                st[i, $2]++
                if ($2 == "S") {
                    if ($3 == "do_epoll_wait") ep[i]++
                    else oth[i]++
                }
                break
            }
        }
    }
    END {
        for (i = 1; i <= nc; i++) {
            span = (i in te) ? te[i] - tb[i] : 0
            rt = (i in s1 && span > 0) ? (s1[i] - s0[i]) * 100.0 / (span * 1e9) : 0
            tk = (i in t1 && span > 0) ? (t1[i] - t0[i]) * 100.0 / (span * 100) : 0
            vsum = 0; vmax = 0
            if (i in v1 && span > 0 && v0[i] != "-") {
                k = split(v0[i], a, ",")
                split(v1[i], b, ",")
                for (j = 1; j <= k; j++) {
                    d = b[j] - a[j]
                    vsum += d
                    if (d > vmax) vmax = d
                }
            }
            pool = (i in p1 && span > 0) ? (p1[i] - p0[i]) * 100.0 / (span * 1e9) : 0
            printf "%s\t%s\t%s\t%d\t%d\t%d\t%d\t%d\t%d\t%.1f\t%.1f\t%.0f\t%.0f\t%.1f\t%d\n",
                fs[i], md[i], th[i], n[i] + 0, st[i, "R"] + 0, st[i, "S"] + 0,
                st[i, "D"] + 0, ep[i] + 0, oth[i] + 0, rt, tk,
                (span > 0 ? vsum * 100.0 / (span * 100) : 0),
                (span > 0 ? vmax * 100.0 / (span * 100) : 0),
                pool, (i in pnl ? pnl[i] + 0 : 0)
        }
    }' "$run/cells.tsv" "$run/samples.txt" >"$run/percell.tsv"

# The table: mean over rounds per (fs, mode, threads), plus the scaling ratio
# (ops/s at T threads over ops/s at the smallest thread count) and the server
# side evidence: fs worker runtime%, its state split, the share of its samples
# parked in the event loop, and the guest vCPU CPU%.
awk -F'\t' '
    FILENAME ~ /cells.tsv$/ {
        key = $1 SUBSEP $2 SUBSEP $3
        n[key]++
        ops_s[key] += $6; cpus[key] += $9; p50[key] += $7; p99[key] += $8; gc[key] += $13
        order[key] = 1
        next
    }
    {
        key = $1 SUBSEP $2 SUBSEP $3
        nn[key] += $4; R[key] += $5; S[key] += $6; D[key] += $7
        ep[key] += $8; oth[key] += $9
        rt[key] += $10; tk[key] += $11; vc[key] += $12; vcm[key] += $13
        pl[key] += $14; pln[key] += $15
        ncell[key]++
    }
    END {
        split("virtiofs shm", fss, " ")
        split("open read stat statwarm readdirplus", mds, " ")
        printf "%-8s %-11s %7s %6s %10s %8s %7s %7s %9s %10s %11s %8s %9s %9s %9s %9s\n",
            "fs", "mode", "threads", "cells", "ops/s", "ratio", "p50us", "p99us",
            "benchcpu%", "fs_worker%", "R/S/D %", "epoll%", "vcpusum%", "busyvcpu%", "gcpu%", "pool%"
        for (fi = 1; fi <= 2; fi++) for (mi = 1; mi <= 5; mi++) {
            base = 0
            for (ti = 1; ti <= 128; ti++) {
                key = fss[fi] SUBSEP mds[mi] SUBSEP ti
                if (!(key in order)) continue
                mean = (n[key] > 0) ? ops_s[key] / n[key] : 0
                if (base == 0) base = mean
                n_ = ncell[key] + 0
                tol = R[key] + S[key] + D[key] + 0
                if (n_ > 0 && tol > 0) {
                    r = 100 * R[key] / tol
                    s_ = 100 * S[key] / tol
                    d = 100 * D[key] / tol
                    epq = 100 * ep[key] / tol
                    rtv = rt[key] / n_
                    vcv = vc[key] / n_
                    vcmv = vcm[key] / n_
                } else {
                    r = s_ = d = epq = rtv = vcv = vcmv = 0
                }
                printf "%-8s %-11s %7d %6d %10.0f %8s %7.1f %7.1f %9.0f %10.1f %4.0f/%3.0f/%3.0f %8.0f %9.0f %9.0f %9.0f %9.1f\n",
                    fss[fi], mds[mi], ti, n[key], mean,
                    (base > 0 ? sprintf("%.2fx", mean / base) : "-"),
                    (n[key] > 0 ? p50[key] / n[key] : 0),
                    (n[key] > 0 ? p99[key] / n[key] : 0),
                    (n[key] > 0 ? cpus[key] / n[key] : 0),
                    rtv, r, s_, d, epq, vcv, vcmv, (n[key] > 0 ? gc[key] / n[key] : 0),
                    (ncell[key] > 0 ? pl[key] / ncell[key] : 0)
            }
        }
    }' "$run/cells.tsv" "$run/percell.tsv" | tee "$run/table.txt"

echo
echo "clock offset guest->host: ${offset}s (start ping), ${offset_end}s (end ping)"
echo "raw cells:  $run/results/cells.txt"
echo "sampler:    $run/samples.txt  (4 Hz; ts state wchan ticks run_ns wait_ns vcsw vcpu_ticks_csv tid pool_ticks pool_run_ns pool_threads, "-" when vcpu=1)"
echo "per-cell:   $run/percell.tsv  (fs mode threads samples R S D epoll other fs_runtime% tick% vcpusum% busyvcpu% pool_runtime% pool_threads)"
