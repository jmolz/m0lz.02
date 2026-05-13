FROM node:22-slim
WORKDIR /app
COPY package.json ./
RUN npm install --ignore-scripts
COPY . .
CMD ["node", "src/server.ts"]
