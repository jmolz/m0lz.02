import { Router } from "express";
import { User } from "../models/user";

export const users = Router();

users.get("/", (_req, res) => {
  res.json({ users: [new User("fixture@example.invalid")] });
});
