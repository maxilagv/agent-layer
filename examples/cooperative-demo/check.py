"""Toy gateway with a negative control; contains no actual service or credentials."""


def storage():
    return 200


def gateway(authenticated):
    if not authenticated:
        return 401
    return storage()


assert gateway(False) == 401, "anonymous access must be rejected"
assert gateway(True) == 200, "positive control must reach storage"
print("anonymous=401; authenticated=200")
