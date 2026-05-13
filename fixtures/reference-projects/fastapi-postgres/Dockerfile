FROM python:3.12-slim
WORKDIR /app
COPY pyproject.toml ./
COPY app app
CMD ["python", "-m", "uvicorn", "app.main:app"]
