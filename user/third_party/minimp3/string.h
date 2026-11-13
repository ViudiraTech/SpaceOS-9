#ifndef SPACEOS_MINIMP3_STRING_H
#define SPACEOS_MINIMP3_STRING_H
/* The decoder only needs the memory primitives provided by splibc. */
void *memcpy(void *destination, const void *source, unsigned long count);
void *memmove(void *destination, const void *source, unsigned long count);
void *memset(void *destination, int value, unsigned long count);
#endif
