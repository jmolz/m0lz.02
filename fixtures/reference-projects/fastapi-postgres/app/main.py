from fastapi import FastAPI

from .db import list_users

app = FastAPI()


@app.get("/users")
def users():
    return {"users": list_users()}
