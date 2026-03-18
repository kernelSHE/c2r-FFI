/* MVP: vars, if/while, simple ops, return */

int compute(int x, int n) {
    int acc = 0;
    int i = 0;
    if (n > 0) {
        while (i < n) {
            acc = acc + x;
            i = i + 1;
        }
    }
    return acc;
}
