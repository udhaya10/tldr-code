from models import Maximum, User


def persist():
    # x: Maximum = Maximum() must never bind x.
    max: Maximum = Maximum()
    x: User = User()
    x.save()
    return max
