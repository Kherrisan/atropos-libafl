#define _GNU_SOURCE
#include <fcntl.h>
#include <jni.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/shm.h>
#include <time.h>
#include <unistd.h>

static uint8_t *bitmap;
static uint32_t bitmap_size;
static uint32_t epoch_seen;
static uint64_t *hit_stats;

static void ensure_hit_stats(void) {
	int fd;
	void *mapped;
	if (hit_stats != NULL) {
		return;
	}
	fd = open("/tmp/nyx-hit-stats", O_CREAT | O_RDWR, 0644);
	if (fd < 0) {
		return;
	}
	if (ftruncate(fd, 16) != 0) {
		close(fd);
		return;
	}
	mapped = mmap(NULL, 16, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	close(fd);
	if (mapped == MAP_FAILED) {
		return;
	}
	hit_stats = (uint64_t *)mapped;
}

/* Same path as PHP pcov: one byte store into the SysV segment Nyx registered
   as the coverage bitmap. Reattach after each restore; a mapping taken before
   the snapshot does not stay on the pages the fuzzer reads. */

static uint32_t read_edge_epoch(void) {
	char buf[16];
	ssize_t n;
	int fd = open("/tmp/nyx-edge-epoch", O_RDONLY);
	if (fd < 0) {
		return 0;
	}
	n = read(fd, buf, sizeof(buf) - 1);
	close(fd);
	if (n <= 0) {
		return 0;
	}
	buf[n] = '\0';
	return (uint32_t)strtoul(buf, NULL, 10);
}

static int attach_bitmap(void) {
	const char *id_text = getenv("SHM_ID");
	const char *size_text = getenv("BITMAP_SIZE");
	FILE *status;
	int shm_id;
	unsigned long size;
	void *mapped;

	if (id_text == NULL || size_text == NULL || id_text[0] == '\0' || size_text[0] == '\0') {
		return 0;
	}
	shm_id = atoi(id_text);
	size = strtoul(size_text, NULL, 10);
	if (size == 0 || (size & (size - 1)) != 0) {
		return 0;
	}
	mapped = shmat(shm_id, NULL, 0);
	status = fopen("/tmp/nyx-bitmap-jni.txt", "w");
	if (mapped == (void *)-1) {
		if (status != NULL) {
			fprintf(status, "shmat-failed source=SHM_ID id=%d size=%lu\n", shm_id, size);
			fclose(status);
		}
		return 0;
	}
	bitmap = (uint8_t *)mapped;
	bitmap_size = (uint32_t)size;
	if (status != NULL) {
		fprintf(status, "mapped source=SHM_ID id=%d size=%u\n", shm_id, bitmap_size);
		fclose(status);
	}
	return 1;
}

static void refresh_bitmap(void) {
	struct timespec start;
	struct timespec stop;
	uint32_t epoch;
	uint64_t elapsed;
	int new_epoch;
	clock_gettime(CLOCK_MONOTONIC, &start);
	epoch = read_edge_epoch();
	clock_gettime(CLOCK_MONOTONIC, &stop);
	elapsed = (uint64_t)(stop.tv_sec - start.tv_sec) * 1000000000ull +
		(uint64_t)(stop.tv_nsec - start.tv_nsec);
	new_epoch = bitmap != NULL && epoch != epoch_seen;
	if (bitmap != NULL && epoch == epoch_seen) {
		ensure_hit_stats();
		if (hit_stats != NULL) {
			hit_stats[0]++;
			hit_stats[1] += elapsed;
		}
		return;
	}
	if (bitmap != NULL && bitmap != (uint8_t *)-1) {
		shmdt(bitmap);
		bitmap = NULL;
		bitmap_size = 0;
	}
	if (attach_bitmap()) {
		epoch_seen = epoch;
	}
	ensure_hit_stats();
	if (hit_stats != NULL) {
		if (new_epoch) {
			hit_stats[0] = 0;
			hit_stats[1] = 0;
		}
		hit_stats[0]++;
		hit_stats[1] += elapsed;
	}
}

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *vm, void *reserved) {
	(void)vm;
	(void)reserved;
	attach_bitmap();
	epoch_seen = read_edge_epoch();
	return JNI_VERSION_1_8;
}

JNIEXPORT void JNICALL Java_runtime_NyxBitmap_hit(JNIEnv *env, jclass cls, jint id) {
	(void)env;
	(void)cls;
	refresh_bitmap();
	if (bitmap == NULL || bitmap_size == 0) {
		return;
	}
	bitmap[((uint32_t)id) & (bitmap_size - 1)] = 1;
}
