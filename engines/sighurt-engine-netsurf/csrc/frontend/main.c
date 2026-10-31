/* Stub so the NetSurf makefile can link. build.rs drops any object whose name
 * contains "main", so this is not in libnscore.a and does not clash with sig. */
int main(void)
{
	return 0;
}
