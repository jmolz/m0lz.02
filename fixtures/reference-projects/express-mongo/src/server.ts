import express from "express";
import { users } from "./routes/users";

const app = express();
app.use("/users", users);
app.listen(process.env.PORT ?? 3000);
