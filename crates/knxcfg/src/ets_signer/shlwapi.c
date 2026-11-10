/*
 * PathRelativePathTo for Mono on Linux: ETS's signing assembly calls
 * shlwapi.dll!PathRelativePathTo to name each file relative to the project
 * directory. Built by knxcfg as libshlwapi.so (see ets_sign.rs).
 *
 * Like Windows: components are compared ignoring case, '/' and '\' both
 * separate, the result uses '\', a file directly in `from` gives ".\name",
 * one further down "sub\name", one elsewhere "..\name".
 */
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <strings.h>
#include <wchar.h>
#include <stdlib.h>

#define MAX_PATH_LEN 4096
#define MAX_PARTS 256

static int is_sep(char c)
{
	return c == '/' || c == '\\';
}

/* Split `path` (modified) into its components; "." and empty ones are dropped. */
static int split(char *path, char **parts)
{
	int n = 0;
	char *p = path;

	while (*p) {
		while (is_sep(*p))
			*p++ = '\0';
		if (!*p)
			break;
		char *start = p;
		while (*p && !is_sep(*p))
			p++;
		if (*p)
			*p++ = '\0';
		if (strcmp(start, ".") == 0)
			continue;
		if (n == MAX_PARTS)
			return -1;
		parts[n++] = start;
	}
	return n;
}

static int append(char *out, size_t cap, size_t *len, const char *s)
{
	size_t l = strlen(s);

	if (*len + l + 1 > cap)
		return 0;
	memcpy(out + *len, s, l + 1);
	*len += l;
	return 1;
}

static int relative(char *out, size_t cap, const char *from, const char *to)
{
	char f[MAX_PATH_LEN], t[MAX_PATH_LEN];
	char *fp[MAX_PARTS], *tp[MAX_PARTS];
	size_t len = 0;
	int nf, nt, common = 0, i;

	if (strlen(from) >= sizeof(f) || strlen(to) >= sizeof(t) || cap == 0)
		return 0;
	strcpy(f, from);
	strcpy(t, to);
	nf = split(f, fp);
	nt = split(t, tp);
	if (nf < 0 || nt < 0)
		return 0;
	while (common < nf && common < nt && strcasecmp(fp[common], tp[common]) == 0)
		common++;

	out[0] = '\0';
	for (i = common; i < nf; i++)
		if (!append(out, cap, &len, "..\\"))
			return 0;
	if (common == nf && !append(out, cap, &len, ".\\"))
		return 0;
	for (i = common; i < nt; i++) {
		if (i > common && !append(out, cap, &len, "\\"))
			return 0;
		if (!append(out, cap, &len, tp[i]))
			return 0;
	}
	return 1;
}

/* BOOL is a 32-bit int. The output buffer has MAX_PATH (260) characters. */
int32_t PathRelativePathToA(char *path, const char *from, uint32_t attr_from,
			    const char *to, uint32_t attr_to)
{
	(void)attr_from;
	(void)attr_to;
	if (!path || !from || !to)
		return 0;
	return relative(path, 260, from, to);
}

int32_t PathRelativePathTo(char *path, const char *from, uint32_t attr_from,
			   const char *to, uint32_t attr_to)
{
	return PathRelativePathToA(path, from, attr_from, to, attr_to);
}

/* Mono passes UTF-16 strings for CharSet.Unicode; wchar_t is 32 bits on Linux. */
static int from_utf16(char *out, size_t cap, const uint16_t *s)
{
	size_t n = 0;

	for (; *s; s++) {
		if (*s > 0x7F || n + 1 >= cap)
			return 0;	/* project file names are ASCII */
		out[n++] = (char)*s;
	}
	out[n] = '\0';
	return 1;
}

int32_t PathRelativePathToW(uint16_t *path, const uint16_t *from, uint32_t attr_from,
			    const uint16_t *to, uint32_t attr_to)
{
	char f[MAX_PATH_LEN], t[MAX_PATH_LEN], r[260];
	size_t i;

	(void)attr_from;
	(void)attr_to;
	if (!path || !from || !to || !from_utf16(f, sizeof(f), from) || !from_utf16(t, sizeof(t), to))
		return 0;
	if (!relative(r, sizeof(r), f, t))
		return 0;
	for (i = 0; r[i]; i++)
		path[i] = (uint16_t)(unsigned char)r[i];
	path[i] = 0;
	return 1;
}
