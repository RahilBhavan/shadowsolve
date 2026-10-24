FROM python:3.12-slim
WORKDIR /app
COPY pyproject.toml ./
COPY research ./research
COPY tests/mock_rpc.py ./mock_rpc.py
RUN pip install --no-cache-dir --disable-pip-version-check 'pytest==8.3.5'
ENV PYTHONPATH=/app/research
RUN useradd --create-home --uid 10001 researcher
USER researcher
CMD ["python3", "-m", "pytest", "research"]
