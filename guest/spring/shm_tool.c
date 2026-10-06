#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/shm.h>

int main(int argc, char **argv) {
	if (argc == 3 && strcmp(argv[1], "create") == 0) {
		unsigned long size = strtoul(argv[2], NULL, 10);
		int id = shmget(IPC_PRIVATE, size, IPC_CREAT | IPC_EXCL | 0600);
		if (id < 0) {
			perror("shmget");
			return 1;
		}
		void *mapped = shmat(id, NULL, 0);
		if (mapped == (void *)-1) {
			perror("shmat");
			return 1;
		}
		memset(mapped, 0, size);
		shmdt(mapped);
		printf("%d\n", id);
		return 0;
	}
	if (argc == 4 && strcmp(argv[1], "nonzero") == 0) {
		int id = atoi(argv[2]);
		unsigned long size = strtoul(argv[3], NULL, 10);
		uint8_t *mapped = shmat(id, NULL, 0);
		if (mapped == (void *)-1) {
			perror("shmat");
			return 1;
		}
		unsigned long count = 0;
		for (unsigned long i = 0; i < size; i++) {
			if (mapped[i] != 0) {
				count++;
			}
		}
		shmdt(mapped);
		printf("%lu\n", count);
		return 0;
	}
	fprintf(stderr, "usage: %s create SIZE | nonzero ID SIZE\n", argv[0]);
	return 2;
}
