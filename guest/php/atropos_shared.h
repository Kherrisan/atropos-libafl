#ifndef ATROPOS_SHARED_H
#define ATROPOS_SHARED_H

#include <stdint.h>

#define ATROPOS_SHARED_MAGIC UINT64_C(0x4154524f504f5331)
#define ATROPOS_SHARED_VERSION 1u
#define ATROPOS_SHARED_INPUT_CAPACITY (1024u * 1024u)
#define ATROPOS_SHARED_OUTPUT_CAPACITY (8u * 1024u * 1024u)

enum atropos_shared_state {
	ATROPOS_SHARED_STARTING = 0,
	ATROPOS_SHARED_BOOT_READY = 1,
	ATROPOS_SHARED_REQUEST_READY = 2,
	ATROPOS_SHARED_REQUEST_RUNNING = 3,
	ATROPOS_SHARED_REQUEST_DONE = 4,
	ATROPOS_SHARED_REQUEST_FAILED = 5,
};

enum atropos_shared_flags {
	ATROPOS_SHARED_OUTPUT_TRUNCATED = 1u << 0,
};

struct atropos_shared_memory {
	uint64_t magic;
	uint32_t version;
	volatile uint32_t state;
	uint32_t sequence;
	uint32_t input_length;
	uint32_t output_length;
	int32_t http_status;
	uint32_t flags;
	char input[ATROPOS_SHARED_INPUT_CAPACITY];
	char output[ATROPOS_SHARED_OUTPUT_CAPACITY];
};

static inline uint32_t atropos_shared_load_state(const struct atropos_shared_memory *shared) {
	return __atomic_load_n(&shared->state, __ATOMIC_ACQUIRE);
}

static inline void atropos_shared_store_state(struct atropos_shared_memory *shared, uint32_t state) {
	__atomic_store_n(&shared->state, state, __ATOMIC_RELEASE);
}

#endif
