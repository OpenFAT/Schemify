/* No libc on wasm32: Nim's C output includes <string.h> for these, and schemify.h (compiled
   in schemify.c) defines them. */
#include <stddef.h>
void *memset(void *dst, int c, size_t n);
void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
size_t strlen(const char *s);
